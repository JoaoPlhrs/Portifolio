; Instalador do J-Menseger para Windows.
;
; Monta-se com:  makensis packaging\windows\j-menseger.nsi
; Espera encontrar o executável já compilado em
; target\x86_64-pc-windows-gnu\release\j-menseger.exe
;
; O aplicativo é um único executável: não acompanha nenhuma DLL porque usa só
; bibliotecas que já vêm no Windows.

Unicode true

!define NOME        "J-Menseger"
!define VERSAO      "0.2.0"
!define PUBLICADOR  "J-Menseger"
!define EXECUTAVEL  "j-menseger.exe"
!define CHAVE       "Software\Microsoft\Windows\CurrentVersion\Uninstall\J-Menseger"

!include "MUI2.nsh"
!include "FileFunc.nsh"

Name "${NOME}"
OutFile "..\..\target\J-Menseger-${VERSAO}-instalador.exe"
InstallDir "$PROGRAMFILES64\${NOME}"
InstallDirRegKey HKLM "Software\${NOME}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

VIProductVersion "${VERSAO}.0"
VIAddVersionKey "ProductName"     "${NOME}"
VIAddVersionKey "FileDescription" "Instalador do ${NOME}"
VIAddVersionKey "FileVersion"     "${VERSAO}"
VIAddVersionKey "ProductVersion"  "${VERSAO}"
VIAddVersionKey "LegalCopyright"  "Licença MIT"

!define MUI_ICON   "j-menseger.ico"
!define MUI_UNICON "j-menseger.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXECUTAVEL}"
!define MUI_FINISHPAGE_RUN_TEXT "Abrir o ${NOME}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "PortugueseBR"
!insertmacro MUI_LANGUAGE "English"

Section "Programa" SecPrograma
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; Se o aplicativo estiver aberto, o arquivo não pode ser substituído.
  ClearErrors
  File "..\..\target\x86_64-pc-windows-gnu\release\${EXECUTAVEL}"
  IfErrors 0 +3
    MessageBox MB_ICONSTOP "Não foi possível gravar o programa. Feche o ${NOME} e tente de novo."
    Abort

  WriteUninstaller "$INSTDIR\Desinstalar.exe"
  WriteRegStr HKLM "Software\${NOME}" "InstallDir" "$INSTDIR"

  ; Entrada em "Aplicativos instalados" do Windows.
  WriteRegStr   HKLM "${CHAVE}" "DisplayName"     "${NOME}"
  WriteRegStr   HKLM "${CHAVE}" "DisplayIcon"     "$INSTDIR\${EXECUTAVEL}"
  WriteRegStr   HKLM "${CHAVE}" "DisplayVersion"  "${VERSAO}"
  WriteRegStr   HKLM "${CHAVE}" "Publisher"       "${PUBLICADOR}"
  WriteRegStr   HKLM "${CHAVE}" "UninstallString" '"$INSTDIR\Desinstalar.exe"'
  WriteRegStr   HKLM "${CHAVE}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKLM "${CHAVE}" "NoModify" 1
  WriteRegDWORD HKLM "${CHAVE}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${CHAVE}" "EstimatedSize" "$0"

  CreateDirectory "$SMPROGRAMS\${NOME}"
  CreateShortCut "$SMPROGRAMS\${NOME}\${NOME}.lnk" "$INSTDIR\${EXECUTAVEL}"
  CreateShortCut "$SMPROGRAMS\${NOME}\Desinstalar.lnk" "$INSTDIR\Desinstalar.exe"
SectionEnd

Section "Atalho na área de trabalho" SecAtalho
  CreateShortCut "$DESKTOP\${NOME}.lnk" "$INSTDIR\${EXECUTAVEL}"
SectionEnd

LangString DESC_Programa ${LANG_PORTUGUESEBR} "O aplicativo e o desinstalador."
LangString DESC_Atalho   ${LANG_PORTUGUESEBR} "Cria um atalho na área de trabalho."
LangString DESC_Programa ${LANG_ENGLISH} "The application and its uninstaller."
LangString DESC_Atalho   ${LANG_ENGLISH} "Creates a shortcut on the desktop."

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPrograma} $(DESC_Programa)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAtalho}   $(DESC_Atalho)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  Delete "$INSTDIR\${EXECUTAVEL}"
  Delete "$INSTDIR\Desinstalar.exe"
  RMDir  "$INSTDIR"

  Delete "$SMPROGRAMS\${NOME}\${NOME}.lnk"
  Delete "$SMPROGRAMS\${NOME}\Desinstalar.lnk"
  RMDir  "$SMPROGRAMS\${NOME}"
  Delete "$DESKTOP\${NOME}.lnk"

  DeleteRegKey HKLM "${CHAVE}"
  DeleteRegKey HKLM "Software\${NOME}"

  ; A conta e as conversas NÃO são apagadas: a chave da conta vive em
  ; %APPDATA%\J-Menseger e, perdida, não há como recuperar a conta. Quem
  ; quiser apagar de verdade remove essa pasta à mão.
  MessageBox MB_OK|MB_ICONINFORMATION \
    "O ${NOME} foi removido.$\n$\nSua conta e suas conversas continuam em:$\n$APPDATA\${NOME}$\n$\nApague essa pasta se quiser remover também a conta. Sem as 12 palavras, uma conta apagada não volta."
SectionEnd
