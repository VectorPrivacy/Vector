; Opt the installer process out of the modal "Bad Image" hard-error dialog
; (0xc000007b): third-party hooks inject DLLs into the 32-bit NSIS process and a
; corrupt one raises it per-load. 0x8003 = SEM_FAILCRITICALERRORS |
; SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX;
; inherited by child processes (WebView2 bootstrapper, app launch).
!macro NSIS_HOOK_PREINSTALL
  System::Call 'kernel32::SetErrorMode(i 0x8003)'
!macroend
