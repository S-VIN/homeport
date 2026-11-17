# Per-user installer: package.py passes STAGE (the files), OUTPUT and VERSION.
Unicode true
!include "MUI2.nsh"
Name "rproxy"
OutFile "${OUTPUT}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
InstallDir "$LOCALAPPDATA\Programs\rproxy"
InstallDirRegKey HKCU "Software\rproxy" "InstallDir"

# Windows locks running executables: ask the user to quit rproxy first.
!macro CHECK_RUNNING UN
Function ${UN}CheckRunning
  retry:
  StrCpy $1 "rproxy.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running
  StrCpy $1 "mihomo.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running done
  running:
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "rproxy is running. Quit it from its tray icon (Quit rproxy), then click Retry." /SD IDCANCEL IDRETRY retry
  Abort
  done:
FunctionEnd
Function ${UN}Locked
  StrCpy $0 ""
  IfFileExists "$INSTDIR\$1" 0 free
  ClearErrors
  FileOpen $2 "$INSTDIR\$1" a
  IfErrors 0 +3
  StrCpy $0 "locked"
  Goto free
  FileClose $2
  free:
FunctionEnd
!macroend
!insertmacro CHECK_RUNNING ""
!insertmacro CHECK_RUNNING "un."

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section
  SetOutPath "$INSTDIR"
  Call CheckRunning
  File /r "${STAGE}\*"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\rproxy" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayName" "rproxy"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  CreateShortcut "$SMPROGRAMS\rproxy.lnk" "$INSTDIR\rproxy.exe"
SectionEnd

Section "Uninstall"
  Call un.CheckRunning
  Delete "$SMPROGRAMS\rproxy.lnk"
  Delete "$INSTDIR\rproxy.exe"
  Delete "$INSTDIR\mihomo.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\licenses"
  # Remove the payload of older installations too.
  RMDir /r "$INSTDIR\assets"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\rproxy"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\rproxy"
SectionEnd
