Var WavePreviousAssociation
Var WaveKeepAssociation

!macro NSIS_HOOK_PREINSTALL
  ; On an in-place update, keep the association from before WAVE was first installed.
  ReadRegStr $WavePreviousAssociation SHCTX "Software\Classes\.wave" ""
  StrCpy $WaveKeepAssociation 0
  ${If} $WavePreviousAssociation == "WAVE.Project"
    ReadRegStr $WavePreviousAssociation SHCTX "Software\Classes\.wave" "WAVE.Project_backup"
  ${Else}
    ReadRegStr $R0 SHCTX "Software\Classes\WAVE.Project\shell\open\command" ""
    ${If} $R0 != ""
      StrCpy $WaveKeepAssociation 1
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Tauri registers WAVE.Project and copies the icon resource before this hook.
  WriteRegStr SHCTX "Software\Classes\.wave" "WAVE.Project_backup" "$WavePreviousAssociation"
  ${If} $WaveKeepAssociation == 1
    WriteRegStr SHCTX "Software\Classes\.wave" "" "$WavePreviousAssociation"
  ${EndIf}
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
