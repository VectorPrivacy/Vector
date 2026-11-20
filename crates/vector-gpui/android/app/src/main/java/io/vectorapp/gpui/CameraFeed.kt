package io.vectorapp.gpui

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.ImageFormat
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.media.ImageReader
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.Surface
import android.view.WindowManager

/**
 * A camera as a stream of YUV frames for Rust (crates/vector-gpui/src/callvideo/camera.rs): each
 * frame's planes go to [nativeFrame] while the image is held, so Rust copies them before it is
 * closed. One feed at a time, named by the id Rust gave it.
 */
object CameraFeed {
    private const val TAG = "vector"

    private var thread: HandlerThread? = null
    private var handler: Handler? = null
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null
    @Volatile private var running = 0L

    @JvmStatic external fun nativeFrame(
        id: Long,
        y: java.nio.ByteBuffer, u: java.nio.ByteBuffer, v: java.nio.ByteBuffer,
        yRow: Int, uvRow: Int, uvPixel: Int,
        width: Int, height: Int, rotation: Int,
    )

    @JvmStatic external fun nativeLost(id: Long)

    /** The answer to the camera permission prompt, from MainActivity. */
    @JvmStatic external fun nativePermission(granted: Boolean)

    const val PERMISSION_REQUEST = 9877

    /** The camera facing that way, else the first there is. */
    private fun pick(manager: CameraManager, front: Boolean): String? {
        val want = if (front) CameraCharacteristics.LENS_FACING_FRONT else CameraCharacteristics.LENS_FACING_BACK
        val ids = manager.cameraIdList
        return ids.firstOrNull { manager.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == want }
            ?: ids.firstOrNull()
    }

    /** The output size nearest `width`x`height` by area, of those the camera gives as YUV. */
    private fun size(chars: CameraCharacteristics, width: Int, height: Int): Size {
        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
        val sizes = map?.getOutputSizes(ImageFormat.YUV_420_888)?.toList().orEmpty()
        val area = width.toLong() * height
        return sizes.minByOrNull { Math.abs(it.width.toLong() * it.height - area) } ?: Size(width, height)
    }

    /** Degrees the sensor's picture turns clockwise to stand upright on the display as it is now. */
    private fun rotation(context: Context, chars: CameraCharacteristics, front: Boolean): Int {
        val sensor = chars.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
        @Suppress("DEPRECATION")
        val display = (context.getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay.rotation
        val device = when (display) {
            Surface.ROTATION_90 -> 90
            Surface.ROTATION_180 -> 180
            Surface.ROTATION_270 -> 270
            else -> 0
        }
        return if (front) (sensor + device) % 360 else (sensor - device + 360) % 360
    }

    /** Opens a camera for feed `id`; false when there is none or it can't be reached. */
    @SuppressLint("MissingPermission")
    @JvmStatic
    @Synchronized
    fun start(context: Context, id: Long, front: Boolean, width: Int, height: Int): Boolean {
        stopLocked()
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as? CameraManager ?: return false
        return try {
            val cameraId = pick(manager, front) ?: return false
            val chars = manager.getCameraCharacteristics(cameraId)
            val facingFront = chars.get(CameraCharacteristics.LENS_FACING) == CameraCharacteristics.LENS_FACING_FRONT
            val out = size(chars, width, height)
            val worker = HandlerThread("camera").also { it.start() }
            val handler = Handler(worker.looper)
            val images = ImageReader.newInstance(out.width, out.height, ImageFormat.YUV_420_888, 3)
            images.setOnImageAvailableListener({ r ->
                val image = r.acquireLatestImage() ?: return@setOnImageAvailableListener
                try {
                    if (running == id) {
                        val p = image.planes
                        nativeFrame(
                            id, p[0].buffer, p[1].buffer, p[2].buffer,
                            p[0].rowStride, p[1].rowStride, p[1].pixelStride,
                            image.width, image.height, rotation(context, chars, facingFront),
                        )
                    }
                } finally {
                    image.close()
                }
            }, handler)
            thread = worker
            this.handler = handler
            reader = images
            running = id
            manager.openCamera(cameraId, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    synchronized(CameraFeed) {
                        if (running != id) {
                            camera.close()
                            return
                        }
                        device = camera
                    }
                    val target = images.surface
                    @Suppress("DEPRECATION")
                    camera.createCaptureSession(listOf(target), object : CameraCaptureSession.StateCallback() {
                        override fun onConfigured(s: CameraCaptureSession) {
                            synchronized(CameraFeed) {
                                if (running != id) {
                                    s.close()
                                    return
                                }
                                session = s
                            }
                            try {
                                val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
                                    addTarget(target)
                                    set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, Range(15, 30))
                                }
                                s.setRepeatingRequest(request.build(), null, handler)
                            } catch (e: Exception) {
                                Log.w(TAG, "camera request: $e")
                                lost(id)
                            }
                        }

                        override fun onConfigureFailed(s: CameraCaptureSession) {
                            Log.w(TAG, "camera session failed")
                            lost(id)
                        }
                    }, handler)
                }

                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    lost(id)
                }

                override fun onError(camera: CameraDevice, error: Int) {
                    Log.w(TAG, "camera error $error")
                    camera.close()
                    lost(id)
                }
            }, handler)
            true
        } catch (e: Exception) {
            Log.w(TAG, "camera: $e")
            stopLocked()
            false
        }
    }

    private fun lost(id: Long) {
        val current = synchronized(CameraFeed) { running == id }
        if (current) {
            stop(id)
            nativeLost(id)
        }
    }

    /** Ends feed `id`, if it is still the one running. */
    @JvmStatic
    @Synchronized
    fun stop(id: Long) {
        if (running == id) {
            stopLocked()
        }
    }

    /** Closes on the camera's own thread, so no frame is mid-copy when its reader goes. */
    private fun stopLocked() {
        running = 0L
        val s = session
        val d = device
        val r = reader
        val t = thread
        val h = handler
        session = null
        device = null
        reader = null
        thread = null
        handler = null
        val close = Runnable {
            try {
                s?.close()
            } catch (_: Exception) {
            }
            try {
                d?.close()
            } catch (_: Exception) {
            }
            r?.close()
            t?.quitSafely()
        }
        if (h == null || !h.post(close)) {
            close.run()
        }
    }
}
