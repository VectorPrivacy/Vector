; Per-user installer for the GPUI client, and what its in-app updater runs (backend/install.rs):
; /P installs with no pages, /R starts the app once it is in place.
;   makensis -DSOURCE=<Vector GPUI.exe> -DICON=<icon.ico> -DVERSION=<x.y.z> -DOUTFILE=<setup.exe> installer.nsi
Unicode true
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!define APP "Vector GPUI"
!define EXE "Vector GPUI.exe"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\VectorGPUI"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${ICON}"
UninstallIcon "${ICON}"

Page directory
Page instfiles
UninstPage uninstConfirm
UninstPage instfiles

Var Relaunch

Function .onInit
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/P" $1
  ${IfNot} ${Errors}
    SetSilent silent
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/R" $1
  ${IfNot} ${Errors}
    StrCpy $Relaunch 1
  ${EndIf}
FunctionEnd

Section
  SetOutPath "$INSTDIR"
  ; The copy being replaced may still be closing: its exe stays locked until it has.
  StrCpy $2 0
  retry:
  ClearErrors
  Delete "$INSTDIR\${EXE}"
  ${If} ${Errors}
  ${AndIf} $2 < 100
    IntOp $2 $2 + 1
    Sleep 200
    Goto retry
  ${EndIf}
  File "/oname=${EXE}" "${SOURCE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortCut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "Vector Privacy"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${If} $Relaunch == 1
    Exec '"$INSTDIR\${EXE}"'
  ${EndIf}
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  DeleteRegKey HKCU "${UNINST_KEY}"
SectionEnd
