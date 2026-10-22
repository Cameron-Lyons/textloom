# Prepares the evaluation VM for Textloom's native checks. `cargo winvm provision`
# runs it elevated through the agent; -Verify prints what is installed.
param([switch]$Verify)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$root = 'C:\textloom-qa'
$downloads = Join-Path $root 'downloads'
$mesa = Join-Path $root 'mesa'
$nvdaVersion = '2026.2'
# The digest the hosted reader probe verifies (scripts/native-qa/windows_reader.ps1).
$nvdaSha256 = 'F3F8D29974A88D687B3C4809BE192219EC579C5BDABCDA5AAF53635288BCA824'
# Input methods: US English, Microsoft Japanese IME, Microsoft Pinyin, German, French (AZERTY), Russian.
$languages = 'en-US', 'ja-JP', 'zh-Hans-CN', 'de-DE', 'fr-FR', 'ru-RU'
# NVDA 2026 is 64-bit; earlier releases installed under Program Files (x86).
$nvdaPaths = "$env:ProgramFiles\NVDA\nvda.exe", "${env:ProgramFiles(x86)}\NVDA\nvda.exe"

function Step([string]$Name, [scriptblock]$Body) {
    Write-Output "== $Name"
    & $Body
}

function Download([string]$Uri, [string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Path
    }
    return $Path
}

# Windows PowerShell turns redirected native stderr into errors, which 'Stop'
# would make fatal; native programs report failure through exit codes instead.
function Invoke-Native([scriptblock]$Body) {
    $ErrorActionPreference = 'Continue'
    & $Body 2>&1 | ForEach-Object { Write-Host "$_" }
    return $LASTEXITCODE
}

function Invoke-Msys([string]$Command) {
    $env:MSYSTEM = 'UCRT64'
    return Invoke-Native { & 'C:\msys64\usr\bin\bash.exe' -lc $Command }
}

if ($Verify) {
    $os = Get-CimInstance Win32_OperatingSystem
    $summary = [ordered]@{
        windows = "$($os.Caption) $($os.Version)"
        input_methods = @((Get-WinUserLanguageList) | ForEach-Object { "$($_.LanguageTag): $($_.InputMethodTips -join ', ')" })
        mesa = "$(& 'C:\msys64\usr\bin\bash.exe' -lc 'pacman -Q mingw-w64-ucrt-x86_64-mesa' 2>$null)"
        mesa_dlls = @(Get-ChildItem -LiteralPath $mesa -Filter *.dll | ForEach-Object Name)
        powershell7 = if (Test-Path 'C:\Program Files\PowerShell\7\pwsh.exe') {
            (& 'C:\Program Files\PowerShell\7\pwsh.exe' -NoProfile -Command '$PSVersionTable.PSVersion.ToString()') } else { $null }
        nvda = @($nvdaPaths | Where-Object { Test-Path -LiteralPath $_ }) | Select-Object -First 1
    }
    $summary | ConvertTo-Json
    exit 0
}

New-Item -ItemType Directory -Force $downloads, $mesa | Out-Null

Step 'Evaluation activation' {
    # Until it activates online, the evaluation reports an expired license and
    # shuts down every hour. Activation starts its 90 days.
    $code = Invoke-Native { cscript //nologo "$env:SystemRoot\System32\slmgr.vbs" /ato }
    if ($code -ne 0) { throw 'Windows evaluation activation failed' }
    Invoke-Native { cscript //nologo "$env:SystemRoot\System32\slmgr.vbs" /xpr } | Out-Null
}

Step 'Power, screen saver, and Defender exclusion' {
    Invoke-Native {
        powercfg /change monitor-timeout-ac 0
        powercfg /change standby-timeout-ac 0
        powercfg /hibernate off
    } | Out-Null
    Set-ItemProperty 'HKCU:\Control Panel\Desktop' -Name ScreenSaveActive -Value 0
    # Let processes started by the agent take the foreground, as on a CI runner;
    # the hosted reader probe focuses the editor itself. Applies at the next logon.
    Set-ItemProperty 'HKCU:\Control Panel\Desktop' -Name ForegroundLockTimeout -Value 0 -Type DWord
    Add-MpPreference -ExclusionPath $root
}

Step 'Input methods and keyboard layouts' {
    $list = New-WinUserLanguageList $languages[0]
    foreach ($language in $languages | Select-Object -Skip 1) { $list.Add($language) }
    Set-WinUserLanguageList $list -Force
    # Microsoft Pinyin asks once whether to send typing to Bing for cloud
    # candidates; answering in advance keeps the prompt from taking a check's keys.
    # Fixed candidate ranking keeps learning from reordering candidates between runs.
    $pinyin = 'HKCU:\Software\Microsoft\InputMethod\Settings\CHS'
    if (-not (Test-Path $pinyin)) { New-Item -Path $pinyin -Force | Out-Null }
    Set-ItemProperty $pinyin -Name 'Enable Cloud Candidate' -Value 0 -Type DWord
    Set-ItemProperty $pinyin -Name 'Enable Dynamic Candidate Ranking' -Value 0 -Type DWord
    # Basic typing features for the CJK IMEs come from Windows Update.
    foreach ($capability in 'Language.Basic~~~ja-JP~0.0.1.0', 'Language.Basic~~~zh-CN~0.0.1.0') {
        try {
            Add-WindowsCapability -Online -Name $capability | Out-Null
            Write-Output "installed $capability"
        } catch {
            Write-Output "could not install $capability`: $($_.Exception.Message)"
        }
    }
}

Step 'MSYS2 and the Mesa software renderer' {
    if (-not (Test-Path 'C:\msys64\usr\bin\bash.exe')) {
        $archive = Download 'https://repo.msys2.org/distrib/msys2-x86_64-latest.sfx.exe' (Join-Path $downloads 'msys2.sfx.exe')
        Invoke-Native { & $archive '-y' '-oC:\' } | Out-Null
    }
    # The first update replaces the MSYS2 runtime and exits; the second finishes.
    Invoke-Msys 'pacman -Syuu --noconfirm --disable-download-timeout' | Out-Null
    if ((Invoke-Msys 'pacman -Syuu --noconfirm --disable-download-timeout') -ne 0) { throw 'MSYS2 update failed' }
    if ((Invoke-Msys 'pacman -S --needed --noconfirm --disable-download-timeout mingw-w64-ucrt-x86_64-mesa') -ne 0) {
        throw 'Mesa installation failed'
    }
    Copy-Item 'C:\msys64\ucrt64\bin\opengl32.dll', 'C:\msys64\ucrt64\bin\libgallium_wgl.dll' -Destination $mesa -Force
}

Step 'PowerShell 7' {
    if (-not (Test-Path 'C:\Program Files\PowerShell\7\pwsh.exe')) {
        $release = Invoke-RestMethod -UseBasicParsing 'https://api.github.com/repos/PowerShell/PowerShell/releases/latest'
        $asset = $release.assets | Where-Object { $_.name -like 'PowerShell-*-win-x64.msi' } | Select-Object -First 1
        $msi = Download $asset.browser_download_url (Join-Path $downloads $asset.name)
        if ((Get-AuthenticodeSignature -LiteralPath $msi).Status -ne 'Valid') { throw 'PowerShell installer signature is invalid' }
        $process = Start-Process msiexec.exe -ArgumentList "/i `"$msi`" /quiet /norestart ADD_PATH=1" -PassThru
        if (-not $process.WaitForExit(900000) -or $process.ExitCode -ne 0) { throw "PowerShell installation failed ($($process.ExitCode))" }
    }
}

Step "NVDA $nvdaVersion" {
    if (-not ($nvdaPaths | Where-Object { Test-Path -LiteralPath $_ })) {
        $installer = Download "https://github.com/nvaccess/nvda/releases/download/release-$nvdaVersion/nvda_$nvdaVersion.exe" `
            (Join-Path $downloads "nvda_$nvdaVersion.exe")
        if ((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash -ne $nvdaSha256) { throw 'NVDA installer digest mismatch' }
        if ((Get-AuthenticodeSignature -LiteralPath $installer).Status -ne 'Valid') { throw 'NVDA installer signature is invalid' }
        $process = Start-Process $installer -ArgumentList '--install-silent' -PassThru
        if (-not $process.WaitForExit(900000)) { throw 'NVDA installation timed out' }
    }
    # Checks start NVDA themselves; it must not run at logon, where it would take keys.
    Get-Process -Name nvda, nvda_slave -ErrorAction SilentlyContinue | Stop-Process -Force
    $accessibility = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Accessibility'
    $configuration = (Get-ItemProperty -LiteralPath $accessibility -ErrorAction SilentlyContinue).Configuration
    if ($configuration -like '*nvda*') {
        Set-ItemProperty -LiteralPath $accessibility -Name Configuration -Value (($configuration -split ',' | Where-Object { $_ -ne 'nvda' }) -join ',')
    }
}

Write-Output 'Provisioning finished.'
