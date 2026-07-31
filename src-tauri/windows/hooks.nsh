!macro NSIS_HOOK_PREUNINSTALL
  IfFileExists "$INSTDIR\${MAINBINARYNAME}.exe" 0 codex_launcher_cleanup_done
  ClearErrors
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --uninstall-cleanup' $0
  DetailPrint "Codex Launcher scheduled-task cleanup exit code: $0"
  codex_launcher_cleanup_done:
!macroend
