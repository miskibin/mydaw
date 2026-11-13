Var WavePreviousAssociation

!macro NSIS_HOOK_PREINSTALL
  ; On an in-place update, keep the association from before WAVE was first installed.
  ReadRegStr $WavePreviousAssociation SHCTX "Software\Classes\.wave" ""
  ${If} $WavePreviousAssociation == "WAVE.Project"
    ReadRegStr $WavePreviousAssociation SHCTX "Software\Classes\.wave" "WAVE.Project_backup"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Tauri registers WAVE.Project and copies the icon resource before this hook.
  WriteRegStr SHCTX "Software\Classes\.wave" "WAVE.Project_backup" "$WavePreviousAssociation"
  WriteRegStr SHCTX "Software\Classes\WAVE.Project\DefaultIcon" "" '$\"$INSTDIR\wave-project.ico$\",0'
  !insertmacro UPDATEFILEASSOC
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Preserve any replacement association when Tauri restores the backed-up default.
  ReadRegStr $WavePreviousAssociation SHCTX "Software\Classes\.wave" ""
  ${If} $WavePreviousAssociation != "WAVE.Project"
    WriteRegStr SHCTX "Software\Classes\.wave" "WAVE.Project_backup" "$WavePreviousAssociation"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro UPDATEFILEASSOC
!macroend
