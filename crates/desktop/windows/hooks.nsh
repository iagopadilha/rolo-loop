; Agent Maestro NSIS hooks (tauri.conf.json bundle.windows.nsis.installerHooks).
; After install: record the app and the skill in ~/.loop-harness/config.json and put <install>\plugin\bin (harness, guard) on the user PATH.
; Before uninstall: take that folder off the PATH. The data folder (~/.loop-harness: projects, settings) is the user's and stays.

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Registering Agent Maestro for the harness command line and the Claude Code skill"
  nsExec::ExecToLog '"$INSTDIR\plugin\bin\harness.exe" install --desktop-exe "$INSTDIR\${MAINBINARYNAME}.exe" --path add'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog '"$INSTDIR\plugin\bin\harness.exe" install --path remove'
  Pop $0
!macroend
