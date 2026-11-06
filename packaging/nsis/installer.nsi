; Atelyx Windows 安装器（NSIS 3.x，per-machine 安装）。
; 版本与各输入路径由 scripts/package.sh 经 -D 注入：VERSION / EXE / LICENSE / NOTICES / ICON / OUTFILE / PRESETS。
Unicode true
ManifestDPIAware true
RequestExecutionLevel admin

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef OUTFILE
  !define OUTFILE "Atelyx-setup.exe"
!endif

!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Atelyx"

Name "Atelyx"
BrandingText "Atelyx"
OutFile "${OUTFILE}"
Icon "${ICON}"
UninstallIcon "${ICON}"

; 升级时沿用上次安装目录；首次安装落到 Program Files
InstallDir "$PROGRAMFILES64\Atelyx"
InstallDirRegKey HKLM "Software\Atelyx" "InstallDir"

!include "MUI2.nsh"
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\atelyx.exe"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetShellVarContext all
  SetOutPath "$INSTDIR"
  File "/oname=atelyx.exe" "${EXE}"
  File "/oname=LICENSE" "${LICENSE}"
  File "/oname=THIRD_PARTY_NOTICES.md" "${NOTICES}"

  ; 随包预置插件：装到独立目录，由应用首次启动时装进用户插件根（用户卸载过的不再装回）
  ; 先清旧目录：升级时已不在包里的预置项不能残留（残留会被下次启动重新装进用户插件根）
  RMDir /r "$INSTDIR\preset-plugins"
  SetOutPath "$INSTDIR\preset-plugins"
  File /r "${PRESETS}\*"

  ; 快捷方式面向全体用户：开始菜单 + 桌面
  CreateDirectory "$SMPROGRAMS\Atelyx"
  CreateShortcut "$SMPROGRAMS\Atelyx\Atelyx.lnk" "$INSTDIR\atelyx.exe"
  CreateShortcut "$DESKTOP\Atelyx.lnk" "$INSTDIR\atelyx.exe"

  ; 卸载器与「应用和功能」注册表项；用户数据（%APPDATA%\com.atelyx.desktop）不在卸载范围
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Atelyx" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "Atelyx"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "Atelyx"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\atelyx.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  ; 卸载路径须内嵌引号：默认安装目录 Program Files 含空格，控制面板按原文执行该值
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext all
  Delete "$INSTDIR\atelyx.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\preset-plugins"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Atelyx\Atelyx.lnk"
  RMDir "$SMPROGRAMS\Atelyx"
  Delete "$DESKTOP\Atelyx.lnk"
  DeleteRegKey HKLM "${UNINST_KEY}"
  DeleteRegKey HKLM "Software\Atelyx"
SectionEnd
