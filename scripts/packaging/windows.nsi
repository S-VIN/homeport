# Per-user installer: package.py passes STAGE (the files), OUTPUT and VERSION.
Unicode true
!include "MUI2.nsh"
Name "homeport"
OutFile "${OUTPUT}"
RequestExecutionLevel user
SetCompressor /SOLID lzma
InstallDir "$LOCALAPPDATA\Programs\homeport"
InstallDirRegKey HKCU "Software\homeport" "InstallDir"

# Windows locks running executables: ask the user to quit homeport first.
!macro CHECK_RUNNING UN
Function ${UN}CheckRunning
  retry:
  StrCpy $1 "homeport.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running
  StrCpy $1 "mihomo.exe"
  Call ${UN}Locked
  StrCmp $0 "locked" running done
  running:
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "homeport is running. Quit it from its tray icon (Quit homeport), then click Retry." /SD IDCANCEL IDRETRY retry
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
  WriteRegStr HKCU "Software\homeport" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\homeport" "DisplayName" "homeport"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\homeport" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\homeport" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  CreateShortcut "$SMPROGRAMS\homeport.lnk" "$INSTDIR\homeport.exe"
SectionEnd

Section "Uninstall"
  Call un.CheckRunning
  Delete "$SMPROGRAMS\homeport.lnk"
  Delete "$INSTDIR\homeport.exe"
  Delete "$INSTDIR\mihomo.exe"
  Delete "$INSTDIR\preset.sql"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\licenses"
  # Remove the payload of older installations too.
  RMDir /r "$INSTDIR\assets"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\homeport"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\homeport"
SectionEnd
