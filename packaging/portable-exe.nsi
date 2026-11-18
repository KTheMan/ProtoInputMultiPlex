Unicode true
RequestExecutionLevel user
!include "MUI2.nsh"
!include "x64.nsh"
Name "ProtoInputMultiPlex Portable"
OutFile "${OUTPUT_FILE}"
InstallDir "$DOCUMENTS\ProtoInputMultiPlex Portable"
SetCompressor /SOLID lzma
ShowInstDetails show
InstallButtonText "Extract"
VIProductVersion "0.1.0.0"
VIFileVersion "0.1.0.0"
VIAddVersionKey /LANG=1033 "ProductName" "ProtoInputMultiPlex Portable"
VIAddVersionKey /LANG=1033 "ProductVersion" "0.1.0"
VIAddVersionKey /LANG=1033 "CompanyName" "ProtoInputMultiPlex contributors"
VIAddVersionKey /LANG=1033 "FileDescription" "Extract the ProtoInputMultiPlex portable folder"
VIAddVersionKey /LANG=1033 "FileVersion" "0.1.0.0"
VIAddVersionKey /LANG=1033 "OriginalFilename" "ProtoInputMultiPlex-0.1.0-windows-x64-portable.exe"
VIAddVersionKey /LANG=1033 "LegalCopyright" "Copyright (c) 2026 ProtoInputMultiPlex contributors"
!define MUI_ABORTWARNING
!define MUI_DIRECTORYPAGE_TEXT_TOP "Choose a new folder inside an existing folder. Keep the extracted files together."
!define MUI_DIRECTORYPAGE_TEXT_DESTINATION "Portable folder"
!define MUI_INSTFILESPAGE_FINISHHEADER_TEXT "Extraction complete"
!define MUI_FINISHPAGE_TITLE "ProtoInputMultiPlex is ready"
!define MUI_FINISHPAGE_TEXT "Keep this folder together. To open it again, run launch-gui.cmd from the extracted folder."
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Launch ProtoInputMultiPlex"
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchPortable
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_LANGUAGE "English"
Function .onInit
 SetShellVarContext current
 ${IfNot} ${RunningX64}
   MessageBox MB_ICONSTOP "ProtoInputMultiPlex Portable requires 64-bit Windows." /SD IDOK
   SetErrorLevel 1
   Abort
 ${EndIf}
FunctionEnd
Function .onVerifyInstDir
 System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR") i.r0'
 StrCmp $0 -1 available
 Abort
 available:
FunctionEnd
!macro VerifyMember FILE_PATH FILE_SIZE
 ClearErrors
 FileOpen $1 "$INSTDIR\${FILE_PATH}" r
 IfErrors failed
 StrCpy $2 "-1"
 FileSeek $1 0 END $2
 FileClose $1
 StrCmp $2 "${FILE_SIZE}" +2
 Goto failed
!macroend
Section "Extract portable files"
 ; Atomically claim the new leaf before writing.
 System::Call 'kernel32::CreateDirectoryW(w "$INSTDIR", p 0) i.r0'
 StrCmp $0 0 unavailable
 SetOutPath "$INSTDIR"
 SetOverwrite off
 ClearErrors
 File /r "${BUNDLE_DIR}/*"
 IfErrors failed
 !include "${VERIFY_INCLUDE}"
 SetErrorLevel 0
 Goto complete
 unavailable:
 MessageBox MB_ICONSTOP "Choose a new folder inside an existing folder." /SD IDOK
 SetErrorLevel 1
 Abort
 failed:
 MessageBox MB_ICONSTOP "Extraction could not finish. Choose another new folder to retry." /SD IDOK
 SetErrorLevel 1
 Abort
 complete:
SectionEnd
Function LaunchPortable
 SetOutPath "$INSTDIR"
 ClearErrors
 ExecShell "open" "$INSTDIR\launch-gui.cmd"
 IfErrors 0 done
 MessageBox MB_ICONSTOP "Open launch-gui.cmd from the extracted folder to retry launching." /SD IDOK
 SetErrorLevel 1
 done:
FunctionEnd

