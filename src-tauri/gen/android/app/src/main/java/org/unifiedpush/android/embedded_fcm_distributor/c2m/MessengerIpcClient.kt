package org.unifiedpush.android.embedded_fcm_distributor.c2m

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.util.Log
import java.util.concurrent.TimeoutException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.retry
import kotlinx.coroutines.flow.shareIn
import kotlinx.coroutines.launch
import org.unifiedpush.android.embedded_fcm_distributor.ACTION_C2M_REGISTER
import org.unifiedpush.android.embedded_fcm_distributor.GSF_PACKAGE

internal object MessengerIpcClient {

    private val TAG = MessengerIpcClient::class.java.simpleName
    lateinit var applicationContext: Context

    val serviceIntent = Intent(ACTION_C2M_REGISTER).apply {
        `package` = GSF_PACKAGE
    }

    private fun send(context: Context, request: IpcRequest) = callbackFlow<Result<Bundle>> {
        applicationContext = context.applicationContext
        val handler = object : Handler(Looper.getMainLooper()) {
            override fun handleMessage(msg: Message) {
                Log.d(TAG, "Handling message with keys ${msg.data.keySet()}")
                if (msg.data.getBoolean("unsupported", false)) {
                    trySend(Result.failure(Exception("Not supported by GmsCore")))
                } else {
                    trySend(Result.success(msg.data))
                }
            }
        }
        val messenger = Messenger(handler)
        remoteService.first()?.send(request.message(context, messenger))
        val timeout = launch {
            delay(10_000)
            trySend(Result.failure(TimeoutException("No response after 10sec")))
        }
        awaitClose {
            timeout.cancel()
            handler.removeCallbacksAndMessages(null)
        }
    }

    fun sendOneWay(context: Context, request: IpcRequest.OneWayRequest): Flow<Result<Boolean>> = send(context, request).map { res ->
        res.mapCatching {
            if (it.containsKey("ack")) {
                it.getBoolean("ack")
            } else {
                throw Exception("Invalid data")
            }
        }
    }

    fun sendTwoWay(context: Context, request: IpcRequest.TwoWayRequest): Flow<Result<Bundle>> = send(context, request).map { res ->
        res.mapCatching {
            if (it.containsKey("data")) {
                it.getBundle("data")!!
            } else {
                throw Exception("Invalid data")
            }
        }
    }

    private val remoteService = callbackFlow<Messenger?> {
        val context = applicationContext
        val serviceConnection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, binder: IBinder) {
                if ("android.os.IMessenger" != binder.interfaceDescriptor) {
                    throw SecurityException("Invalid interface descriptor")
                }
                Log.d(TAG, "C2M service Connected")
                trySend(Messenger(binder))
            }

            override fun onServiceDisconnected(name: ComponentName) {
                Log.d(TAG, "C2M service Disconnected")
                cancel("connection to service lost")
            }

            override fun onNullBinding(name: ComponentName?) {
                Log.i(TAG, "Received null binding")
                cancel(
                    "Connection to C2M service failed",
                    NullPointerException("Received null binding")
                )
            }

            override fun onBindingDied(name: ComponentName?) {
                Log.i(TAG, "Binding died")
                cancel("Binding died")
            }
        }

        try {
            Log.i(TAG, "Connecting to C2M service")
            val serviceBound = context.bindService(
                serviceIntent,
                serviceConnection,
                Context.BIND_AUTO_CREATE
            )
            check(serviceBound) { "C2M service not found" }
        } catch (e: SecurityException) {
            cancel("Connection to Play Service failed", e)
            Log.w(TAG, e.message, e)
        } catch (e: IllegalStateException) {
            cancel("Connection to Play Service failed", e)
            Log.w(TAG, e.message, e)
        } catch (e: Exception) {
            Log.w(TAG, e.message, e)
        }

        awaitClose {
            Log.d(TAG, "Unbinding from C2M service")
            context.unbindService(serviceConnection)
        }
    }.retry(3) {
        Log.d(TAG, "Retry connection after 1sec")
        delay(1_000L)
        return@retry true
    }
        .catch { emit(null) }
        .shareIn(
            CoroutineScope(Dispatchers.IO),
            SharingStarted.WhileSubscribed(1_000L, 0),
            1
        )
}
