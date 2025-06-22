package io.vectorapp;

import android.app.Activity;
import android.content.pm.PackageManager;
import androidx.annotation.NonNull;

public class PermissionHandler {
    static {
        System.loadLibrary("vector_lib");
    }
    
    // Native callback method
    private static native void onPermissionResult(int requestCode, boolean granted);
    
    // Native handler for onPermissionResult
    public static void handlePermissionResult(
        int requestCode,
        @NonNull String[] permissions,
        @NonNull int[] grantResults
    ) {
        if (requestCode == 9876) { // AUDIO_PERMISSION_REQUEST_CODE
            boolean granted = grantResults.length > 0 && 
                            grantResults[0] == PackageManager.PERMISSION_GRANTED;
            onPermissionResult(requestCode, granted);
        }
    }
}