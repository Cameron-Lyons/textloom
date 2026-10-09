# SPDX-License-Identifier: MIT
# Run only on a disposable hosted Windows GUI runner. Never installs NVDA.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$NativeExecutable,
    [string]$OutputDirectory = "native-qa-observations",
    [int]$TimeoutSeconds = 270
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
if (-not $IsWindows) { throw "This probe requires Windows." }
if ($env:GITHUB_ACTIONS -ne "true" -or $env:RUNNER_OS -ne "Windows") {
    throw "This probe requires a disposable GitHub Actions Windows runner."
}
if ($TimeoutSeconds -lt 60 -or $TimeoutSeconds -gt 300) {
    throw "Probe timeout must be between 60 and 300 seconds."
}

$NativeExecutable = (Resolve-Path -LiteralPath $NativeExecutable).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$TaskRoot = Join-Path ([IO.Path]::GetTempPath()) ("textloom-nvda-" + [guid]::NewGuid().ToString("N"))
$Profile = Join-Path $TaskRoot "profile"
$Portable = Join-Path $TaskRoot "portable"
$EventsPath = Join-Path $Profile "reader-events.jsonl"
$RequestPath = Join-Path $Profile "reader-request.json"
$ReportPath = Join-Path $OutputDirectory "windows-reader-native.json"
$Deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
$Revision = $env:TEXTLOOM_QA_REVISION
if (-not $Revision) { $Revision = (& git rev-parse HEAD).Trim() }
if ($Revision -notmatch "^[0-9a-f]{40}$") { throw "A full source revision is required." }
$NativeProcess = $null
$ReaderProcess = $null
$VerifiedReader = $null
$LauncherProcess = $null
$ReaderId = 0
$ReaderHandle = [IntPtr]::Zero
$Window = [IntPtr]::Zero
$RequestId = 0
$Failure = $null
$OwnedProcessesRemaining = @()
$Checks = [ordered]@{}
$Evidence = [ordered]@{
    schema_version = 1
    source_revision = $Revision
    reader = "NVDA 2026.2"
    installer_sha256 = "f3f8d29974a88d687b3c4809be192219ec579c5bdabcda5aaf53635288bca824"
    speech_evidence = "reader_generated_synthesis_queue"
    audible_speech_verified = $false
    physical_keyboard_verified = $false
    teardown_order = "native_before_reader"
    close_with_reader_active_verified = $false
    manual_signoff = "not_recorded"
    fixture_content_in_capture = $true
    checks = $Checks
    succeeded = $false
    cleanup_succeeded = $false
}

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class TextloomReaderInput {
    public sealed class WindowInfo {
        public string Handle;
        public uint ProcessId;
        public string Title, Class;
        public long ExtendedStyle;
        public bool Visible, ToolWindow, NoActivate;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct KEYBDINPUT {
        public ushort wVk, wScan;
        public uint dwFlags, time;
        public UIntPtr dwExtraInfo;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct MOUSEINPUT {
        public int dx, dy;
        public uint mouseData, dwFlags, time;
        public UIntPtr dwExtraInfo;
    }
    [StructLayout(LayoutKind.Explicit)]
    private struct INPUTUNION {
        [FieldOffset(0)] public KEYBDINPUT keyboard;
        [FieldOffset(0)] public MOUSEINPUT mouse;
    }
    [StructLayout(LayoutKind.Sequential)]
    private struct INPUT { public uint type; public INPUTUNION data; }
    [DllImport("user32.dll", SetLastError=true)]
    private static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr window, StringBuilder text, int maximum);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    private static extern int GetClassName(IntPtr window, StringBuilder text, int maximum);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")]
    private static extern IntPtr GetWindowLongPtr(IntPtr window, int index);
    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);
    private delegate bool EnumerateWindow(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumerateWindow callback, IntPtr parameter);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("kernel32.dll", SetLastError=true)]
    private static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern bool GetUserObjectInformation(
        IntPtr handle, int index, StringBuilder name, int length, out int needed);

    public static uint ForegroundProcess() {
        uint id;
        GetWindowThreadProcessId(GetForegroundWindow(), out id);
        return id;
    }
    public static uint ReadExitCode(IntPtr process) {
        uint code;
        if (process == IntPtr.Zero || !GetExitCodeProcess(process, out code))
            throw new InvalidOperationException("Cannot read the retained reader process exit status.");
        return code;
    }
    public static WindowInfo DescribeWindow(IntPtr window) {
        uint id;
        GetWindowThreadProcessId(window, out id);
        var title = new StringBuilder(1024);
        var windowClass = new StringBuilder(256);
        GetWindowText(window, title, title.Capacity);
        GetClassName(window, windowClass, windowClass.Capacity);
        long style = GetWindowLongPtr(window, -20).ToInt64();
        return new WindowInfo {
            Handle = "0x" + window.ToInt64().ToString("x"), ProcessId = id,
            Title = title.ToString(), Class = windowClass.ToString(),
            ExtendedStyle = style, Visible = IsWindowVisible(window),
            ToolWindow = (style & 0x80) != 0, NoActivate = (style & 0x08000000) != 0
        };
    }
    public static WindowInfo[] OwnedWindows(uint owner) {
        var result = new List<WindowInfo>();
        EnumWindows((window, parameter) => {
            uint id;
            GetWindowThreadProcessId(window, out id);
            if (id == owner) result.Add(DescribeWindow(window));
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }
    public static bool IsEditorWindow(WindowInfo info, uint owner) {
        return info.ProcessId == owner && info.Visible && !info.ToolWindow && !info.NoActivate
            && info.Title == "TextLoom native example" && info.Class.Length != 0
            && info.Class != "Winit Thread Event Target";
    }
    public static IntPtr FindEditorWindow(uint owner) {
        IntPtr result = IntPtr.Zero;
        EnumWindows((window, parameter) => {
            uint id;
            GetWindowThreadProcessId(window, out id);
            if (id != owner) return true;
            if (IsEditorWindow(DescribeWindow(window), owner)) {
                result = window;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return result;
    }
    public static IntPtr VerifiedForegroundEditor(uint owner) {
        IntPtr window = GetForegroundWindow();
        if (!IsEditorWindow(DescribeWindow(window), owner))
            throw new InvalidOperationException("Foreground window is not the owned editor's real native window.");
        return window;
    }
    public static string InputDesktopName() {
        IntPtr desktop = OpenInputDesktop(0, false, 1); // DESKTOP_READOBJECTS
        if (desktop == IntPtr.Zero)
            throw new InvalidOperationException("No usable input desktop.");
        try {
            int needed;
            var name = new StringBuilder(256);
            if (!GetUserObjectInformation(desktop, 2, name, name.Capacity * 2, out needed))
                throw new InvalidOperationException("Cannot inspect input desktop.");
            return name.ToString();
        } finally { CloseDesktop(desktop); }
    }
    private static INPUT Key(ushort key, bool up, bool unicode=false) {
        return new INPUT {
            type = 1,
            data = new INPUTUNION { keyboard = new KEYBDINPUT {
                wVk = unicode ? (ushort)0 : key,
                wScan = unicode ? key : (ushort)0,
                dwFlags = (up ? 2u : 0u) | (unicode ? 4u : 0u)
                    | (!unicode && (key == 0x2d || key == 0x24 || key == 0x26) ? 1u : 0u)
            }}
        };
    }
    private static void Send(INPUT[] inputs, uint owner) {
        if (ForegroundProcess() != owner)
            throw new InvalidOperationException("Owned editor is not the foreground window.");
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT)));
        if (sent != inputs.Length)
            throw new InvalidOperationException("Native input was rejected or only partly delivered.");
    }
    public static void Chord(uint owner, int[] modifiers, int key) {
        var inputs = new List<INPUT>();
        foreach (int modifier in modifiers) inputs.Add(Key((ushort)modifier, false));
        inputs.Add(Key((ushort)key, false));
        inputs.Add(Key((ushort)key, true));
        for (int i=modifiers.Length-1; i>=0; i--) inputs.Add(Key((ushort)modifiers[i], true));
        Send(inputs.ToArray(), owner);
    }
    public static void TypeX(uint owner) {
        Send(new INPUT[] { Key((ushort)'x', false, true), Key((ushort)'x', true, true) }, owner);
    }
    public static void ReleaseModifiers(uint owner) {
        if (ForegroundProcess() != owner) return;
        SendInput(3, new INPUT[] { Key(0x11, true), Key(0x10, true), Key(0x2d, true) },
            Marshal.SizeOf(typeof(INPUT)));
    }
}
'@

function Assert-Time {
    if ([DateTime]::UtcNow -ge $Deadline) { throw "Native reader probe exceeded its deadline." }
}

function Start-OwnedProcess([string]$File, [string[]]$Arguments, [hashtable]$Environment = @{}) {
    Assert-Time
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $File
    $info.UseShellExecute = $false
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    foreach ($entry in $Environment.GetEnumerator()) { $info.Environment[$entry.Key] = $entry.Value }
    return [Diagnostics.Process]::Start($info)
}

function Wait-Condition([scriptblock]$Condition, [string]$Message, [int]$Seconds = 20) {
    $until = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $until) {
        Assert-Time
        $value = & $Condition
        if ($value) { return $value }
        Start-Sleep -Milliseconds 100
    }
    throw $Message
}

function Get-ReaderEvents {
    if (Test-Path -LiteralPath (Join-Path $Profile "reader-capture-failed.json")) {
        throw "NVDA observation callback failed."
    }
    if (-not (Test-Path -LiteralPath $EventsPath)) { return @() }
    $lines = [IO.File]::ReadAllLines($EventsPath)
    $events = [Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $lines.Length; $index++) {
        if (-not $lines[$index]) { continue }
        try { $events.Add(($lines[$index] | ConvertFrom-Json)) }
        catch {
            # An in-progress append can leave only the final line incomplete.
            if ($index -ne $lines.Length - 1) { throw "Reader capture contains malformed JSON." }
        }
    }
    return $events.ToArray()
}

function Invoke-ReaderRequest([string]$Operation) {
    $script:RequestId++
    $id = $script:RequestId
    $temporary = $RequestPath + ".new"
    [IO.File]::WriteAllText($temporary,
        (@{id = $id; operation = $Operation} | ConvertTo-Json -Compress),
        [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $RequestPath -Force
    return Wait-Condition {
        $found = @(Get-ReaderEvents | Where-Object {
            $_.PSObject.Properties.Name -contains "request_id" -and $_.request_id -eq $id
        })
        if ($found.Count) {
            if ($found[-1].kind -eq "capture_error") {
                throw ("NVDA capture request failed: " + $found[-1].error_type)
            }
            return $found[-1]
        }
    } ("NVDA did not answer " + $Operation) 10
}

function Normalize-Text([string]$Value) {
    return $Value.Replace("`r`n", "`n").Replace("`r", "`n").Normalize()
}

function Assert-EditorForeground {
    if ($NativeProcess.HasExited) { throw "Native editor exited before the probe completed." }
    if ([TextloomReaderInput]::ForegroundProcess() -ne $NativeProcess.Id) {
        throw "Native editor lost foreground focus."
    }
}

function Send-Chord([int[]]$Modifiers, [int]$Key) {
    Assert-Time
    Assert-EditorForeground
    [TextloomReaderInput]::Chord([uint32]$NativeProcess.Id, $Modifiers, $Key)
}

function Wait-Snapshot([scriptblock]$Expected, [string]$Message) {
    return Wait-Condition {
        $snapshot = Invoke-ReaderRequest "snapshot"
        if (& $Expected $snapshot) { return $snapshot }
    } $Message 15
}

function Assert-ReaderSpeech(
    [long]$After, [string[]]$Expected, [string]$Name, [switch]$ExactToken
) {
    if ($ExactToken -and $Expected.Count -ne 1) {
        throw "Exact token speech assertions require one expected token."
    }
    Wait-Condition {
        $events = @(Get-ReaderEvents | Where-Object {
            $_.kind -eq "speech_queued" -and $_.sequence -gt $After
        })
        $text = Normalize-Text (($events | ForEach-Object { $_.text }) -join " ")
        if ($ExactToken) {
            return @($text -split "\s+") -ccontains (Normalize-Text $Expected[0])
        }
        # Synthesis can split words into several strings at language boundaries.
        $joined = $text -replace "\s", ""
        foreach ($part in $Expected) {
            $expectedText = (Normalize-Text $part) -replace "\s", ""
            if (-not $joined.Contains($expectedText)) { return $false }
        }
        return $true
    } ("NVDA did not generate expected speech for " + $Name) 15 | Out-Null
    $Checks[$Name] = $true
}

try {
    New-Item -ItemType Directory -Path $TaskRoot, $Profile -Force | Out-Null
    $Evidence.session_id = [Diagnostics.Process]::GetCurrentProcess().SessionId
    $Evidence.user_interactive = [Environment]::UserInteractive
    $Evidence.input_desktop = [TextloomReaderInput]::InputDesktopName()
    if ($Evidence.session_id -eq 0) { throw "Interactive reader probe cannot use Session 0." }
    $existingReaders = @(Get-Process -Name "nvda", "nvda_noUIAccess" -ErrorAction SilentlyContinue)
    if ($existingReaders.Count) { throw "An existing NVDA process prevents an isolated probe." }
    $Installer = Join-Path $TaskRoot "nvda_2026.2.exe"
    Invoke-WebRequest -Uri "https://github.com/nvaccess/nvda/releases/download/release-2026.2/nvda_2026.2.exe" `
        -OutFile $Installer -TimeoutSec 60
    if ((Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant() `
        -ne $Evidence.installer_sha256) { throw "Official NVDA installer digest mismatch." }
    $signature = Get-AuthenticodeSignature -LiteralPath $Installer
    if ($signature.Status -ne "Valid") { throw "Official NVDA installer signature is invalid." }
    $Evidence.installer_signer = $signature.SignerCertificate.Subject
    $LauncherProcess = Start-OwnedProcess $Installer @(
        "--create-portable-silent", "--portable-path=$Portable",
        "--config-path=$Profile", "--minimal", "--no-sr-flag"
    )
    Wait-Condition { $LauncherProcess.HasExited } "NVDA portable creation timed out." 90 | Out-Null
    if ($LauncherProcess.ExitCode -ne 0) { throw "NVDA portable creation failed." }
    $NvdaExe = Join-Path $Portable "nvda.exe"
    if (-not (Test-Path -LiteralPath $NvdaExe)) { throw "NVDA portable executable is missing." }
    $pluginDirectory = Join-Path $Profile "scratchpad/globalPlugins"
    New-Item -ItemType Directory -Path $pluginDirectory -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "nvda/textloomCapture.py") -Destination $pluginDirectory
    @'
schemaVersion = 24
[general]
language = en
saveConfigurationOnExit = False
askToExit = False
playStartAndExitSounds = False
showWelcomeDialogAtStartup = False
[speech]
synth = silence
[keyboard]
keyboardLayout = desktop
handleInjectedKeys = True
[update]
autoCheck = False
startupNotification = False
allowUsageStats = False
askedAllowUsageStats = True
[development]
enableScratchpadDir = True
[addonStore]
automaticUpdates = disabled
'@ | Set-Content -LiteralPath (Join-Path $Profile "nvda.ini") -Encoding utf8NoBOM

    $NativeProcess = Start-OwnedProcess $NativeExecutable @("--rich-clipboard", "--qa-report", $ReportPath)
    $Window = Wait-Condition {
        $NativeProcess.Refresh()
        if ($NativeProcess.HasExited) { throw "Native editor failed to start." }
        if ($NativeProcess.MainWindowHandle -ne [IntPtr]::Zero) { return $NativeProcess.MainWindowHandle }
    } "Native editor did not create a window." 30
    $Evidence.initial_main_window = [TextloomReaderInput]::DescribeWindow($Window)
    $Window = Wait-Condition {
        $actual = [TextloomReaderInput]::FindEditorWindow([uint32]$NativeProcess.Id)
        if ($actual -ne [IntPtr]::Zero) { return $actual }
    } "Native editor did not create its visible non-tool window." 15
    $Evidence.focus_window = [TextloomReaderInput]::DescribeWindow($Window)
    [TextloomReaderInput]::ShowWindow($Window, 9) | Out-Null
    [TextloomReaderInput]::SetForegroundWindow($Window) | Out-Null
    Wait-Condition {
        [TextloomReaderInput]::ForegroundProcess() -eq $NativeProcess.Id
    } "Cannot focus the owned native editor." 10 | Out-Null
    $ReaderProcess = Start-OwnedProcess $NvdaExe @(
        "--minimal", "--config-path=$Profile", "--lang=en",
        "--log-file=$(Join-Path $OutputDirectory 'nvda.log')", "--log-level=10"
    ) @{TEXTLOOM_NVDA_TARGET_PID = [string]$NativeProcess.Id}
    $ready = Wait-Condition {
        @(Get-ReaderEvents | Where-Object { $_.kind -eq "ready" }) | Select-Object -Last 1
    } "NVDA did not finish startup." 30
    $ReaderId = [int]$ready.reader_pid
    $runningReader = Get-Process -Id $ReaderId
    if (-not $runningReader.Path.StartsWith($Portable + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase)) { throw "Reader readiness came from an unowned process." }
    if ($runningReader.SessionId -ne $Evidence.session_id -or $ready.synth -ne "silence") {
        throw "Reader session or synthesizer does not match the isolated profile."
    }
    $VerifiedReader = if ($ReaderProcess.Id -eq $ReaderId) { $ReaderProcess } else { $runningReader }
    # Get-Process can observe an existing PID lazily. Retain its process handle
    # while it is alive so its actual exit status remains available after exit.
    $ReaderHandle = $VerifiedReader.Handle
    $Evidence.reader_launcher_pid = $ReaderProcess.Id
    $Evidence.reader_runtime_handle_retained = $ReaderHandle -ne [IntPtr]::Zero
    $Evidence.reader_pid = $ReaderId
    $Evidence.native_pid = $NativeProcess.Id
    $Checks.reader_started_in_private_profile = $true
    Assert-EditorForeground
    $baseline = Wait-Snapshot {
        param($snapshot)
        $snapshot.name -eq "Document" -and $snapshot.text.Contains("café") `
            -and $snapshot.text.Contains("日本語")
    } "NVDA did not expose the Document text provider."
    $baselineText = Normalize-Text $baseline.text
    $Evidence.text_provider = $baseline.provider
    $Evidence.text_role = $baseline.role
    Assert-ReaderSpeech 0 @("Document") "ordinary_focus_announcement"

    Send-Chord @(0x11) 0x24 # Ctrl+Home: native caret movement.
    Wait-Snapshot { param($snapshot) $snapshot.caret_at_start } `
        "Native Ctrl+Home did not move NVDA's accessible caret." | Out-Null
    $Checks.native_caret_movement = $true
    $barrier = Invoke-ReaderRequest "barrier"
    Send-Chord @(0x2d) 0x26 # NVDA+Up: read current line.
    Assert-ReaderSpeech $barrier.sequence @("TextLoom") "native_reader_line"

    Send-Chord @(0x11) 0x41 # Ctrl+A: native selection, not a direct provider action.
    Wait-Snapshot {
        param($snapshot)
        -not $snapshot.selection_collapsed -and (Normalize-Text $snapshot.selected_text) -eq $baselineText
    } "Native Ctrl+A did not select the fixture through NVDA's text provider." | Out-Null
    $Checks.native_selection = $true
    $barrier = Invoke-ReaderRequest "barrier"
    Send-Chord @(0x2d, 0x10) 0x26 # NVDA+Shift+Up: announce current selection.
    Assert-ReaderSpeech $barrier.sequence @("TextLoom", "café", "日本語") "native_reader_unicode_selection"

    Assert-EditorForeground
    [TextloomReaderInput]::TypeX([uint32]$NativeProcess.Id)
    Wait-Snapshot {
        param($snapshot)
        (Normalize-Text $snapshot.text).TrimEnd("`n") -eq "x" `
            -and $snapshot.selection_collapsed -and $snapshot.caret_at_end
    } "Native typing did not replace the selected fixture with one character." | Out-Null
    $Checks.native_selection_replacement = $true
    $barrier = Invoke-ReaderRequest "barrier"
    # Prevent a second NVDA+Up from being interpreted as the repeat/spelling gesture.
    Start-Sleep -Milliseconds 650
    Send-Chord @(0x2d) 0x26
    Assert-ReaderSpeech $barrier.sequence @("x") "native_reader_replacement" -ExactToken

    Send-Chord @(0x11) 0x5a # Ctrl+Z.
    Wait-Snapshot {
        param($snapshot)
        (Normalize-Text $snapshot.text) -eq $baselineText `
            -and (Normalize-Text $snapshot.selected_text) -eq $baselineText
    } "Native undo did not restore the complete fixture and selection." | Out-Null
    $Checks.native_undo_document_and_selection = $true
    Send-Chord @(0x11, 0x10) 0x5a # Ctrl+Shift+Z.
    Wait-Snapshot {
        param($snapshot)
        (Normalize-Text $snapshot.text).TrimEnd("`n") -eq "x" -and $snapshot.selection_collapsed
    } "Native redo did not restore the replacement." | Out-Null
    $Checks.native_redo = $true
    Send-Chord @(0x11) 0x5a
    Wait-Snapshot {
        param($snapshot)
        (Normalize-Text $snapshot.text) -eq $baselineText `
            -and (Normalize-Text $snapshot.selected_text) -eq $baselineText
    } "Final native undo did not restore the fixture and selection." | Out-Null
    $Checks.final_fixture_restoration = $true
    $barrier = Invoke-ReaderRequest "barrier"
    Start-Sleep -Milliseconds 650
    Send-Chord @(0x2d, 0x10) 0x26
    Assert-ReaderSpeech $barrier.sequence @("TextLoom", "café", "日本語") "native_reader_restored_selection"
    $captureErrors = @(Get-ReaderEvents | Where-Object { $_.kind -eq "capture_error" })
    if ($captureErrors.Count) { throw "NVDA capture recorded an error." }
    Assert-EditorForeground
    # MainWindowHandle may name Winit's visible, unowned thread tool window.
    # Re-resolve the actual editor from the foreground and verify its identity.
    $Window = [TextloomReaderInput]::VerifiedForegroundEditor([uint32]$NativeProcess.Id)
    $Evidence.close_window = [TextloomReaderInput]::DescribeWindow($Window)
    $Evidence.owned_windows_at_close = [TextloomReaderInput]::OwnedWindows([uint32]$NativeProcess.Id)
    if ($VerifiedReader.HasExited) { throw "NVDA exited before the native close test." }
    $Checks.reader_active_at_native_close = $true
    if (-not [TextloomReaderInput]::PostMessage($Window, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)) {
        throw "Cannot request graceful editor exit."
    }
    Wait-Condition { $NativeProcess.HasExited } "Native editor did not exit gracefully." 15 | Out-Null
    if ($NativeProcess.ExitCode -ne 0) { throw "Native editor reported failure." }
    if ($VerifiedReader.HasExited) { throw "NVDA exited during the native close test." }
    & python scripts/check_native_report.py $ReportPath --revision $Revision --os windows --mode editable --interaction
    if ($LASTEXITCODE -ne 0) { throw "Native report validation failed." }
    $report = Get-Content -LiteralPath $ReportPath -Raw | ConvertFrom-Json
    if ($report.host_input_events.text -lt 1 -or $report.host_input_events.key_press -lt 8 `
        -or $report.observed_state_changes.selection -lt 3 `
        -or $report.final_document.text_utf8_bytes_excluding_paragraph_breaks -ne 200 `
        -or $report.final_document.tlfr_bytes -ne 346 `
        -or $report.final_document.undo_steps -ne 0 -or $report.final_document.redo_steps -ne 1) {
        throw "Native report lacks the expected input and restored fixture evidence."
    }
    $Checks.native_report_validated = $true
    $Evidence.close_with_reader_active_verified = $true
    Invoke-ReaderRequest "quit" | Out-Null
    Wait-Condition { $VerifiedReader.HasExited } "Owned NVDA did not exit gracefully." 15 | Out-Null
    $Evidence.reader_exit_code = [TextloomReaderInput]::ReadExitCode($ReaderHandle)
    if ($Evidence.reader_exit_code -ne 0) { throw "Owned NVDA reported exit failure." }
    $terminated = @(Get-ReaderEvents | Where-Object { $_.kind -eq "terminated" })
    if (-not $terminated.Count) { throw "NVDA plugin did not record orderly termination." }
    $Checks.reader_graceful_exit = $true
    $Evidence.succeeded = $true
}
catch {
    $Failure = $_
    $Evidence.failure = $_.Exception.Message
}
finally {
    # Only this probe's explicit PIDs or processes inside its fresh portable
    # directory are eligible for cleanup. Never use a global NVDA quit command.
    if ($NativeProcess -and -not $NativeProcess.HasExited) {
        [TextloomReaderInput]::ReleaseModifiers([uint32]$NativeProcess.Id)
        if ($Window -ne [IntPtr]::Zero) {
            [TextloomReaderInput]::PostMessage($Window, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
            $NativeProcess.WaitForExit(5000) | Out-Null
        }
        if (-not $NativeProcess.HasExited) { $NativeProcess.Kill($true); $NativeProcess.WaitForExit(5000) | Out-Null }
    }
    if (Test-Path -LiteralPath $Profile) {
        try {
            $script:RequestId++
            $quit = @{id = $script:RequestId; operation = "quit"} | ConvertTo-Json -Compress
            [IO.File]::WriteAllText($RequestPath + ".new", $quit, [Text.UTF8Encoding]::new($false))
            Move-Item -LiteralPath ($RequestPath + ".new") -Destination $RequestPath -Force
        } catch { }
    }
    if ($ReaderProcess) { $ReaderProcess.WaitForExit(5000) | Out-Null }
    $ownedReaders = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -and $_.Path.StartsWith($Portable + [IO.Path]::DirectorySeparatorChar,
            [StringComparison]::OrdinalIgnoreCase) } catch { $false }
    })
    foreach ($process in $ownedReaders) {
        try { $process.Kill($true); $process.WaitForExit(5000) | Out-Null } catch { }
    }
    foreach ($process in @($LauncherProcess, $ReaderProcess, $VerifiedReader)) {
        if ($process -and -not $process.HasExited) {
            try { $process.Kill($true); $process.WaitForExit(5000) | Out-Null } catch { }
        }
    }
    if ($VerifiedReader -and $VerifiedReader.HasExited -and $ReaderHandle -ne [IntPtr]::Zero) {
        try { $Evidence.reader_exit_code = [TextloomReaderInput]::ReadExitCode($ReaderHandle) } catch { }
    }
    $ownedIds = @($NativeProcess, $LauncherProcess, $ReaderProcess, $VerifiedReader) |
        Where-Object { $null -ne $_ } | ForEach-Object { $_.Id }
    $OwnedProcessesRemaining = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        if ($ownedIds -contains $_.Id) { return $true }
        try {
            $_.Path -and $_.Path.StartsWith($Portable + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase)
        } catch { $false }
    })
    $Evidence.cleanup_succeeded = $OwnedProcessesRemaining.Count -eq 0
    $Evidence.owned_processes_remaining = $OwnedProcessesRemaining.Count
    if (Test-Path -LiteralPath $EventsPath) {
        Copy-Item -LiteralPath $EventsPath -Destination (Join-Path $OutputDirectory "nvda-reader-events.jsonl") -Force
    }
    $captureFailure = Join-Path $Profile "reader-capture-failed.json"
    if (Test-Path -LiteralPath $captureFailure) {
        Copy-Item -LiteralPath $captureFailure -Destination $OutputDirectory -Force
    }
    $Evidence | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath (Join-Path $OutputDirectory "windows-reader-evidence.json") -Encoding utf8NoBOM
    if ($Evidence.cleanup_succeeded -and (Test-Path -LiteralPath $TaskRoot)) {
        Remove-Item -LiteralPath $TaskRoot -Recurse -Force
    }
}

if ($Failure) { throw $Failure }
if (-not $Evidence.cleanup_succeeded) { throw "Owned reader/editor processes remained after cleanup." }
Write-Output "Native NVDA focus, Unicode speech, selection, replacement, undo/redo passed"
