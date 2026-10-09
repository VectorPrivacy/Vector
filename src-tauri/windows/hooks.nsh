; Opt the installer process out of the modal "Bad Image" hard-error dialog
; (0xc000007b): third-party hooks inject DLLs into the 32-bit NSIS process and a
; corrupt one raises it per-load. 0x8003 = SEM_FAILCRITICALERRORS |
; SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX;
; inherited by child processes (WebView2 bootstrapper, app launch).
!macro NSIS_HOOK_PREINSTALL
  System::Call 'kernel32::SetErrorMode(i 0x8003)'
!macroend

; Native notifications register the app identity and its toast activator per user
; at runtime (services/native_notify/windows.rs); the activator's CLSID is read back
; from the identity before both go.
!macro NSIS_HOOK_POSTUNINSTALL
  ReadRegStr $0 HKCU "Software\Classes\AppUserModelId\${BUNDLEID}" "CustomActivator"
  StrCmp $0 "" +2
    DeleteRegKey HKCU "Software\Classes\CLSID\$0"
  DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}"
!macroend
