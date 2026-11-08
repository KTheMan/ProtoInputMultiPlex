; Thin per-user Windows setup shell. Product transactions and validation live in Rust.
Unicode True
RequestExecutionLevel user
!include MUI2.nsh
!include LogicLib.nsh
!include x64.nsh
!include FileFunc.nsh
Var NoShellIntegration
!ifndef BUNDLE_DIR
!error "Define BUNDLE_DIR as the fully staged portable distribution"
!endif
!ifndef OUTPUT_FILE
!error "Define OUTPUT_FILE as a new installer output path"
!endif
!ifndef APP_VERSION
!error "Define APP_VERSION from the staged bundle manifest"
!endif
!ifndef APP_VERSION_NUMERIC
!error "Define APP_VERSION_NUMERIC as the bundle's four-part Windows version"
!endif
!ifndef SETUP_FILENAME
!error "Define SETUP_FILENAME from the installer output filename"
!endif
; NSIS embeds this shared version block in setup and its generated uninstaller.
VIProductVersion "${APP_VERSION_NUMERIC}"
VIFileVersion "${APP_VERSION_NUMERIC}"
VIAddVersionKey /LANG=1033 "ProductName" "ProtoInputMultiPlex"
VIAddVersionKey /LANG=1033 "ProductVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "CompanyName" "ProtoInputMultiPlex contributors"
VIAddVersionKey /LANG=1033 "FileDescription" "ProtoInputMultiPlex installation and removal"
VIAddVersionKey /LANG=1033 "FileVersion" "${APP_VERSION}"
VIAddVersionKey /LANG=1033 "InternalName" "ProtoInputMultiPlex setup"
VIAddVersionKey /LANG=1033 "OriginalFilename" "${SETUP_FILENAME}"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright (c) 2026 ProtoInputMultiPlex contributors"
!ifndef SHELL_REG_KEY
!define SHELL_REG_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\ProtoInputMultiPlex"
!endif
!ifndef SHELL_MENU_ROOT
!define SHELL_MENU_ROOT "$SMPROGRAMS\ProtoInputMultiPlex"
!endif
Name "ProtoInputMultiPlex"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\ProtoInputMultiPlex"
; Reuse an existing installation registered under the current product identity.
InstallDirRegKey HKCU "${SHELL_REG_KEY}" "InstallLocation"
ShowInstDetails show
ShowUninstDetails show
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${BUNDLE_DIR}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "ProtoInputMultiPlex requires 64-bit Windows." /SD IDOK
    Abort
  ${EndIf}
  SetShellVarContext current
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/NoShellIntegration" $1
  ${IfNot} ${Errors}
    StrCpy $NoShellIntegration "1"
  ${EndIf}
FunctionEnd

Function ValidateInstallRoot
  ; Only overwrite shell files previously created by this setup family.
  IfFileExists "$INSTDIR\.protoinputmultiplex-shell-install" ownedShell
  IfFileExists "$INSTDIR\maintenance.exe" shellCollision
  IfFileExists "$INSTDIR\Uninstall.exe" shellCollision
  IfFileExists "$INSTDIR\.no-shell-integration" shellCollision initDone
  ownedShell:
  FileOpen $0 "$INSTDIR\.protoinputmultiplex-shell-install" r
  FileRead $0 $1
  FileClose $0
  ${If} $1 != "ProtoInputMultiPlex NSIS 1"
    Goto shellCollision
  ${EndIf}
  ; Upgrades retain the shell integration mode selected by the first setup.
  StrCpy $NoShellIntegration ""
  IfFileExists "$INSTDIR\.no-shell-integration" 0 initDone
  StrCpy $NoShellIntegration "1"
  Goto initDone
  shellCollision:
  MessageBox MB_ICONSTOP "This directory contains shell files not owned by ProtoInputMultiPlex setup. Choose a new installation directory." /SD IDOK
  Abort
  initDone:
FunctionEnd

Section "ProtoInputMultiPlex"
  Call ValidateInstallRoot
  InitPluginsDir
  SetOutPath "$PLUGINSDIR\payload"
  File /r "${BUNDLE_DIR}\*.*"
  ClearErrors
  ExecWait '"$PLUGINSDIR\payload\protoinputmultiplex.exe" installation install --bundle "$PLUGINSDIR\payload" --root "$INSTDIR"' $0
  ${If} ${Errors}
    StrCpy $0 "1"
  ${EndIf}
  ${If} $0 != 0
    SetErrorLevel 1
    MessageBox MB_ICONSTOP "Installation could not complete. Close every ProtoInputMultiPlex window and agent process, then retry. Existing application files and user data are preserved; see INSTALLATION.md for recovery." /SD IDOK
    Abort
  ${EndIf}
  SetOutPath "$INSTDIR"
  FileOpen $0 "$INSTDIR\.protoinputmultiplex-shell-install" w
  FileWrite $0 "ProtoInputMultiPlex NSIS 1"
  FileClose $0
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\payload\protoinputmultiplex.exe" "$INSTDIR\maintenance.exe"
  ${If} ${Errors}
    SetErrorLevel 1
    MessageBox MB_ICONSTOP "Program files were installed, but setup could not save its maintenance tool. Retry setup to finish." /SD IDOK
    Abort
  ${EndIf}
  ClearErrors
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ${If} ${Errors}
    SetErrorLevel 1
    MessageBox MB_ICONSTOP "Program files were installed, but setup could not save its uninstaller. Retry setup to finish." /SD IDOK
    Abort
  ${EndIf}
  ${If} $NoShellIntegration == "1"
    FileOpen $0 "$INSTDIR\.no-shell-integration" w
    FileClose $0
    Goto installed
  ${EndIf}
  CreateDirectory "${SHELL_MENU_ROOT}"
  CreateShortcut "${SHELL_MENU_ROOT}\ProtoInputMultiPlex.lnk" "$INSTDIR\app\protoinputmultiplex.exe" "gui"
  CreateShortcut "${SHELL_MENU_ROOT}\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "${SHELL_REG_KEY}" "DisplayName" "ProtoInputMultiPlex"
  WriteRegStr HKCU "${SHELL_REG_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${SHELL_REG_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${SHELL_REG_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegDWORD HKCU "${SHELL_REG_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${SHELL_REG_KEY}" "NoRepair" 1
  installed:
  SetErrorLevel 0
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  InitPluginsDir
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\maintenance.exe" "$PLUGINSDIR\maintenance.exe"
  ${If} ${Errors}
    SetErrorLevel 1
    MessageBox MB_ICONSTOP "The maintenance tool could not be copied. Re-run setup to repair it, then retry uninstall." /SD IDOK
    Abort
  ${EndIf}
  ClearErrors
  ExecWait '"$PLUGINSDIR\maintenance.exe" installation uninstall --root "$INSTDIR"' $0
  ${If} ${Errors}
    StrCpy $0 "1"
  ${EndIf}
  ${If} $0 != 0
    SetErrorLevel 1
    MessageBox MB_ICONSTOP "Uninstall could not complete. Close ProtoInputMultiPlex and all agent clients, then retry. User data has been preserved." /SD IDOK
    Abort
  ${EndIf}
  IfFileExists "$INSTDIR\.no-shell-integration" skipShell
  ReadRegStr $0 HKCU "${SHELL_REG_KEY}" "InstallLocation"
  ${If} $0 != $INSTDIR
    Goto skipShell
  ${EndIf}
  Delete "${SHELL_MENU_ROOT}\ProtoInputMultiPlex.lnk"
  Delete "${SHELL_MENU_ROOT}\Uninstall.lnk"
  RMDir "${SHELL_MENU_ROOT}"
  DeleteRegKey HKCU "${SHELL_REG_KEY}"
  skipShell:
  Delete "$INSTDIR\.no-shell-integration"
  Delete "$INSTDIR\.protoinputmultiplex-shell-install"
  Delete "$INSTDIR\maintenance.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  SetErrorLevel 0
SectionEnd
