# Guest-side helpers for `cargo winvm-checks`: start the native host with the Mesa
# renderer, read editor state through UI Automation, list top-level windows
# (IME candidate lists among them), and switch input methods. Jobs dot-source
# this file and print one JSON object.

Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class TextloomWin {
    public delegate bool EnumProc(IntPtr window, IntPtr parameter);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int size);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int size);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window, out RECT rect);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr LoadKeyboardLayout(string id, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint thread);
    [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr context);
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string className, string title);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref POINT point);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr window, int x, int y, int width, int height, bool repaint);

    public static int[] ClientOrigin(IntPtr window) {
        var point = new POINT();
        ClientToScreen(window, ref point);
        return new int[] { point.X, point.Y };
    }

    public static List<Dictionary<string, object>> Toplevels() {
        var windows = new List<Dictionary<string, object>>();
        EnumWindows((window, parameter) => {
            if (!IsWindowVisible(window)) return true;
            var name = new StringBuilder(256);
            var title = new StringBuilder(256);
            GetClassName(window, name, name.Capacity);
            GetWindowText(window, title, title.Capacity);
            RECT rect;
            GetWindowRect(window, out rect);
            uint process;
            GetWindowThreadProcessId(window, out process);
            if (rect.Right - rect.Left <= 0 || rect.Bottom - rect.Top <= 0) return true;
            windows.Add(new Dictionary<string, object> {
                { "hwnd", window.ToInt64() }, { "class", name.ToString() }, { "title", title.ToString() },
                { "rect", new int[] { rect.Left, rect.Top, rect.Right - rect.Left, rect.Bottom - rect.Top } },
                { "pid", (int)process },
            });
            return true;
        }, IntPtr.Zero);
        return windows;
    }
}
'@
# Report physical pixels, which match QEMU screenshots at any scale.
[TextloomWin]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null

$QaRoot = 'C:\textloom-qa'
$HostDirectory = Join-Path $QaRoot 'host'
$HostPidFile = Join-Path $QaRoot 'host.pid'
$AE = [System.Windows.Automation.AutomationElement]
$TP = [System.Windows.Automation.TextPattern]
$Endpoint = [System.Windows.Automation.Text.TextPatternRangeEndpoint]
$Unit = [System.Windows.Automation.Text.TextUnit]
$Scope = [System.Windows.Automation.TreeScope]
# Keyboard layout handles for WM_INPUTLANGCHANGEREQUEST.
$InputMethods = @{ 'en-US' = '00000409'; 'ja-JP' = '00000411'; 'zh-CN' = '00000804'; 'de-DE' = '00000407'; 'fr-FR' = '0000040C'; 'ru-RU' = '00000419' }

function Stop-TextloomHost {
    Get-Process -Name 'textloom-native-example' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

function Start-TextloomHost([string[]]$Arguments = @()) {
    Stop-TextloomHost
    $env:PATH = "C:\msys64\ucrt64\bin;$env:PATH"
    $env:GALLIUM_DRIVER = 'llvmpipe'
    $env:LIBGL_ALWAYS_SOFTWARE = 'true'
    $env:GLUTIN_WGL_OPENGL_DLL = Join-Path $HostDirectory 'opengl32.dll'
    $env:RUST_BACKTRACE = '1'
    $start = @{ FilePath = (Join-Path $HostDirectory 'textloom-native-example.exe'); WorkingDirectory = $HostDirectory; PassThru = $true
        RedirectStandardOutput = (Join-Path $QaRoot 'host.stdout.log'); RedirectStandardError = (Join-Path $QaRoot 'host.stderr.log') }
    if ($Arguments.Count) { $start.ArgumentList = $Arguments }
    $process = Start-Process @start
    Set-Content -LiteralPath $HostPidFile -Value $process.Id
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "the native host exited with $($process.ExitCode)" }
        if (Get-HostElement 'Document' 'Edit') {
            # The default window is taller than the 1280x800 screen's work area, which
            # would hide its bottom rows behind the taskbar.
            $transform = (Get-HostWindow).GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
            $transform.Move(40, 16)
            $transform.Resize(1000, 724)
            return $process.Id
        }
        Start-Sleep -Milliseconds 300
    }
    throw 'the native host did not show its document within 60 s'
}

function Get-HostPid { [int](Get-Content -LiteralPath $HostPidFile) }

function Get-HostExit {
    $process = Get-Process -Id (Get-HostPid) -ErrorAction SilentlyContinue
    return [ordered]@{
        running = [bool]$process
        stderr = (Get-Content -Raw -LiteralPath (Join-Path $QaRoot 'host.stderr.log') -ErrorAction SilentlyContinue)
    }
}

function Get-HostWindow {
    $condition = New-Object System.Windows.Automation.PropertyCondition($AE::ProcessIdProperty, (Get-HostPid))
    return $AE::RootElement.FindFirst($Scope::Children, $condition)
}

# Text fields share their names with their labels, so fields are looked up as Edit controls.
function Get-HostElement([string]$Name, [string]$Type = '') {
    $window = Get-HostWindow
    if (-not $window) { return $null }
    $condition = if ($Name) { New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $Name) } else { [System.Windows.Automation.Condition]::TrueCondition }
    if ($Type) {
        $controlType = [System.Windows.Automation.ControlType]::$Type
        $condition = New-Object System.Windows.Automation.AndCondition($condition,
            (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, $controlType)))
    }
    return $window.FindFirst($Scope::Descendants, $condition)
}

function Get-Rect($Rect) {
    if ($Rect.IsEmpty) { return $null }
    return @([int]$Rect.X, [int]$Rect.Y, [int]$Rect.Width, [int]$Rect.Height)
}

function Get-TextState([string]$Name = 'Document') {
    $element = Get-HostElement $Name 'Edit'
    if (-not $element) { throw "no text field named $Name" }
    $pattern = $element.GetCurrentPattern($TP::Pattern)
    $document = $pattern.DocumentRange
    $selection = @($pattern.GetSelection())[0]
    if (-not $selection) {
        return [ordered]@{ text = $document.GetText(-1); selection = $null; selected = $null; focused = [bool]$element.Current.HasKeyboardFocus
            caret_box = $null; caret_at_end = $null; rect = Get-Rect $element.Current.BoundingRectangle }
    }
    $before = $document.Clone()
    $before.MoveEndpointByRange($Endpoint::End, $selection, $Endpoint::Start)
    $start = $before.GetText(-1).Length
    $selected = $selection.GetText(-1)
    # A degenerate range has no box; measure the character after the caret, or before it at the end.
    $caret = $selection.Clone()
    $caret.MoveEndpointByRange($Endpoint::End, $selection, $Endpoint::Start)
    $moved = $caret.MoveEndpointByUnit($Endpoint::End, $Unit::Character, 1)
    if ($moved -eq 0) { $caret.MoveEndpointByUnit($Endpoint::Start, $Unit::Character, -1) | Out-Null }
    $boxes = @($caret.GetBoundingRectangles())
    return [ordered]@{
        text = $document.GetText(-1)
        selection = @($start, ($start + $selected.Length))
        selected = $selected
        focused = [bool]$element.Current.HasKeyboardFocus
        caret_box = if ($boxes.Count) { Get-Rect $boxes[0] } else { $null }
        caret_at_end = ($moved -eq 0)
        rect = Get-Rect $element.Current.BoundingRectangle
    }
}

# A degenerate range at a UTF-16 offset. UIA moves by characters, which can
# span several UTF-16 units, so steps shrink instead of overshooting.
function Get-RangeAt($Pattern, [int]$Offset) {
    $document = $Pattern.DocumentRange
    $range = $document.Clone()
    $range.MoveEndpointByRange($Endpoint::End, $range, $Endpoint::Start)
    $length = 0
    $step = [Math]::Max(1, $Offset)
    while ($length -lt $Offset -and $step -ge 1) {
        $trial = $range.Clone()
        $moved = $trial.Move($Unit::Character, $step)
        if ($moved -eq 0) { break }
        $prefix = $document.Clone()
        $prefix.MoveEndpointByRange($Endpoint::End, $trial, $Endpoint::Start)
        $trialLength = $prefix.GetText(-1).Length
        if ($trialLength -le $Offset) {
            $range = $trial
            $length = $trialLength
            $step = [Math]::Max(1, $Offset - $length)
        } else {
            $step = [Math]::Floor($step / 2)
        }
    }
    return $range
}

# Select UTF-16 offsets [Start, End) of a text field.
function Select-HostRange([int]$Start, [int]$End, [string]$Name = 'Document') {
    $pattern = (Get-HostElement $Name 'Edit').GetCurrentPattern($TP::Pattern)
    $selection = Get-RangeAt $pattern $Start
    $selection.MoveEndpointByRange($Endpoint::End, (Get-RangeAt $pattern $End), $Endpoint::Start)
    $selection.Select()
}

function Get-ElementRect([string]$Name, [string]$Type = '') {
    $element = Get-HostElement $Name $Type
    if ($element) { return Get-Rect $element.Current.BoundingRectangle }
    return $null
}

# Paragraph kinds as AccessKit exposes them: heading, group (body text), and list item.
function Get-ParagraphKinds([string]$Name = 'Document') {
    $children = (Get-HostElement $Name 'Edit').FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)
    return @($children | ForEach-Object { [ordered]@{ kind = $_.Current.LocalizedControlType; rect = Get-Rect $_.Current.BoundingRectangle } })
}

function Get-ParagraphStyle {
    $box = (Get-HostWindow).FindFirst($Scope::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)))
    return $box.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
}

function Get-Status {
    $window = Get-HostWindow
    $texts = $window.FindAll($Scope::Descendants,
        (New-Object System.Windows.Automation.PropertyCondition($AE::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)))
    return @($texts | ForEach-Object { $_.Current.Name })
}

function Get-Tree([int]$Depth = 4) {
    function Walk($Element, [int]$Level) {
        $current = $Element.Current
        $node = [ordered]@{
            type = $current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            name = $current.Name
            localized = $current.LocalizedControlType
            rect = Get-Rect $current.BoundingRectangle
            patterns = @($Element.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName -replace 'PatternIdentifiers\.Pattern$', '' })
        }
        if ($Level -lt $Depth) {
            $children = $Element.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)
            if ($children.Count) { $node.children = @($children | ForEach-Object { Walk $_ ($Level + 1) }) }
        }
        return $node
    }
    return Walk (Get-HostWindow) 0
}

function Get-Toplevels {
    $names = @{}
    Get-Process | ForEach-Object { $names[$_.Id] = $_.ProcessName }
    return @([TextloomWin]::Toplevels() | ForEach-Object {
        [ordered]@{ hwnd = $_['hwnd']; class = $_['class']; title = $_['title']; rect = $_['rect']; process = $names[$_['pid']] }
    })
}

# Switch the input method of the host's window (or the foreground window when
# no host runs) and wait until its thread reports the new layout.
function Set-InputMethod([string]$Language) {
    $layout = [TextloomWin]::LoadKeyboardLayout($InputMethods[$Language], 1)
    $window = [TextloomWin]::GetForegroundWindow()
    $hostWindow = $null
    try { $hostWindow = Get-HostWindow } catch { }
    if ($hostWindow) { $window = [IntPtr]$hostWindow.Current.NativeWindowHandle }
    $wanted = $InputMethods[$Language].Substring(4)
    for ($attempt = 0; $attempt -lt 3; $attempt++) {
        # WM_INPUTLANGCHANGEREQUEST
        [TextloomWin]::PostMessage($window, 0x50, [IntPtr]::Zero, $layout) | Out-Null
        $deadline = [DateTime]::UtcNow.AddSeconds(2)
        while ([DateTime]::UtcNow -lt $deadline) {
            $current = Get-InputMethod $window
            if ($current.EndsWith($wanted)) { return $current }
            Start-Sleep -Milliseconds 100
        }
    }
    return Get-InputMethod $window
}

function Get-InputMethod([IntPtr]$Window = [TextloomWin]::GetForegroundWindow()) {
    $process = 0
    $thread = [TextloomWin]::GetWindowThreadProcessId($Window, [ref]$process)
    return ('{0:X8}' -f ([TextloomWin]::GetKeyboardLayout($thread).ToInt64() -band 0xFFFFFFFF))
}

# The winit harness (scripts/windows-vm/winit-harness) prints every event and
# its editor state as JSON lines on stdout.
$WinitDirectory = Join-Path $QaRoot 'winit'
$WinitLog = Join-Path $QaRoot 'winit.stdout.log'

function Stop-WinitHarness {
    Get-Process -Name 'tlqa-winit-harness' -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

function Start-WinitHarness([string[]]$Arguments = @()) {
    Stop-WinitHarness
    $start = @{ FilePath = (Join-Path $WinitDirectory 'tlqa-winit-harness.exe'); WorkingDirectory = $WinitDirectory; PassThru = $true
        RedirectStandardOutput = $WinitLog; RedirectStandardError = (Join-Path $QaRoot 'winit.stderr.log') }
    if ($Arguments.Count) { $start.ArgumentList = $Arguments }
    $process = Start-Process @start
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "the winit harness exited with $($process.ExitCode)" }
        $window = [TextloomWin]::FindWindow([NullString]::Value, 'tlqa-winit')
        if ($window -ne [IntPtr]::Zero -and (Get-WinitEvents | Where-Object { $_.event -eq 'ready' })) {
            [TextloomWin]::MoveWindow($window, 60, 60, 816, 439, $true) | Out-Null
            Start-Sleep -Milliseconds 300
            return [ordered]@{ pid = $process.Id; origin = [TextloomWin]::ClientOrigin($window) }
        }
        Start-Sleep -Milliseconds 300
    }
    throw 'the winit harness did not open its window within 30 s'
}

function Get-WinitEvents([int]$After = 0) {
    if (-not (Test-Path -LiteralPath $WinitLog)) { return @() }
    $stream = [IO.FileStream]::new($WinitLog, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8)
    try { $lines = $reader.ReadToEnd() -split "`r?`n" } finally { $reader.Dispose() }
    return @($lines | Where-Object { $_.StartsWith('{') } | ForEach-Object {
        try { $_ | ConvertFrom-Json } catch { } } | Where-Object { $_ -and $_.seq -gt $After })
}

function Out-Json($Value) {
    $Value | ConvertTo-Json -Depth 12 -Compress
}

# Read native clipboard representations directly; no .NET format synthesis or
# Textloom decoder participates in these observations. Fixtures are bounded.
function Initialize-ClipboardReader {
    if ('TextloomClipboard' -as [type]) { return }
Add-Type -TypeDefinition @'
public static class TextloomClipboard {
    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern bool OpenClipboard(System.IntPtr window);
    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern bool CloseClipboard();
    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern uint EnumClipboardFormats(uint format);
    [System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)] static extern int GetClipboardFormatName(uint format, System.Text.StringBuilder name, int length);
    [System.Runtime.InteropServices.DllImport("user32.dll")] static extern System.IntPtr GetClipboardData(uint format);
    [System.Runtime.InteropServices.DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
    [System.Runtime.InteropServices.DllImport("kernel32.dll")] static extern System.UIntPtr GlobalSize(System.IntPtr memory);
    [System.Runtime.InteropServices.DllImport("kernel32.dll")] static extern System.IntPtr GlobalLock(System.IntPtr memory);
    [System.Runtime.InteropServices.DllImport("kernel32.dll")] static extern bool GlobalUnlock(System.IntPtr memory);
    public sealed class Snapshot {
        public uint Sequence;
        public System.Collections.Generic.Dictionary<string, byte[]> Formats;
    }
    public static Snapshot Read() {
        bool opened = false;
        for (int i = 0; i < 20 && !opened; i++) {
            opened = OpenClipboard(System.IntPtr.Zero);
            if (!opened) System.Threading.Thread.Sleep(50);
        }
        if (!opened) throw new System.Exception("could not open guest clipboard");
        try {
            var result = new System.Collections.Generic.Dictionary<string, byte[]>();
            uint format = 0;
            while ((format = EnumClipboardFormats(format)) != 0) {
                var buffer = new System.Text.StringBuilder(256);
                GetClipboardFormatName(format, buffer, buffer.Capacity);
                string name = format == 13 ? "CF_UNICODETEXT" : buffer.ToString();
                if (name != "CF_UNICODETEXT" && name != "HTML Format" && name != "application/x-textloom-fragment") continue;
                var handle = GetClipboardData(format);
                ulong size = GlobalSize(handle).ToUInt64();
                if (size > 1048576) throw new System.Exception("QA clipboard fixture exceeds 1 MiB");
                var pointer = GlobalLock(handle);
                if (pointer == System.IntPtr.Zero) throw new System.Exception("could not lock clipboard data");
                try {
                    var bytes = new byte[(int)size];
                    System.Runtime.InteropServices.Marshal.Copy(pointer, bytes, 0, bytes.Length);
                    result.Add(name, bytes);
                } finally { GlobalUnlock(handle); }
            }
            // A newer publication must never lend its sequence to these bytes.
            // Sample after delayed formats have rendered, while the lock is held.
            return new Snapshot { Sequence = GetClipboardSequenceNumber(), Formats = result };
        } finally { CloseClipboard(); }
    }
}
'@
}

function Get-NativeClipboard {
    Initialize-ClipboardReader
    $snapshot = [TextloomClipboard]::Read()
    $data = $snapshot.Formats
    $result = [ordered]@{ sequence = $snapshot.Sequence; formats = @($data.Keys); text = $null; html = $null; fragment = $null; tlfr = $null }
    if ($data.ContainsKey('CF_UNICODETEXT')) { $result.text = [Text.Encoding]::Unicode.GetString($data['CF_UNICODETEXT']).TrimEnd([char]0) }
    if ($data.ContainsKey('HTML Format')) {
        $bytes = $data['HTML Format']
        $result.html = [Text.Encoding]::UTF8.GetString($bytes).TrimEnd([char]0)
        $header = [Text.Encoding]::ASCII.GetString($bytes, 0, [Math]::Min(8192, $bytes.Length))
        $offsets = @{}
        foreach ($name in 'StartHTML', 'EndHTML', 'StartFragment', 'EndFragment') {
            $match = [regex]::Match($header, "(?m)^${name}:([0-9]+)")
            if (-not $match.Success) { throw "CF_HTML missing $name" }
            $offsets[$name] = [int]$match.Groups[1].Value
        }
        if ($offsets.StartHTML -gt $offsets.StartFragment -or $offsets.StartFragment -gt $offsets.EndFragment -or $offsets.EndFragment -gt $offsets.EndHTML -or $offsets.EndHTML -gt $bytes.Length) { throw 'CF_HTML byte offsets out of bounds' }
        $utf8 = New-Object Text.UTF8Encoding($false, $true)
        $result.fragment = $utf8.GetString($bytes, $offsets.StartFragment, ($offsets.EndFragment - $offsets.StartFragment))
        $result.offsets = $offsets
    }
    if ($data.ContainsKey('application/x-textloom-fragment')) {
        $bytes = $data['application/x-textloom-fragment']
        if ($bytes.Length -lt 32) { throw 'truncated rich clipboard envelope' }
        $length = [BitConverter]::ToUInt64($bytes, 8)
        if ($length -gt ($bytes.Length - 32)) { throw 'rich clipboard length exceeds native allocation' }
        $result.tlfr = [Convert]::ToBase64String($bytes, 32, [int]$length)
    }
    return $result
}

# Explicitly changes the guest clipboard after a native capture but before its
# caller consumes the pair. A late sequence query would identify the newer offer.
function Test-NativeClipboardSnapshot {
    Initialize-ClipboardReader
    Add-Type -AssemblyName System.Windows.Forms
    [Windows.Forms.Clipboard]::SetText('Textloom snapshot before')
    $before = [TextloomClipboard]::Read()
    [Windows.Forms.Clipboard]::SetText('Textloom snapshot after')
    $after = [TextloomClipboard]::Read()
    $oldText = [Text.Encoding]::Unicode.GetString($before.Formats['CF_UNICODETEXT']).TrimEnd([char]0)
    $newText = [Text.Encoding]::Unicode.GetString($after.Formats['CF_UNICODETEXT']).TrimEnd([char]0)
    return [ordered]@{
        passed = ($before.Sequence -ne $after.Sequence -and $oldText -eq 'Textloom snapshot before' -and $newText -eq 'Textloom snapshot after')
        captured_sequence = $before.Sequence; later_sequence = $after.Sequence
        captured_text = $oldText; later_text = $newText
    }
}

function Get-SelectionAttributes {
    $pattern = (Get-HostElement 'Document' 'Edit').GetCurrentPattern($TP::Pattern)
    $selection = @($pattern.GetSelection())[0]
    $result = [ordered]@{ text = $selection.GetText(-1) }
    foreach ($name in 'ForegroundColor', 'BackgroundColor', 'FontWeight', 'IsItalic', 'UnderlineStyle') {
        $attribute = $TP::("${name}Attribute")
        $value = $selection.GetAttributeValue($attribute)
        $result[$name] = if ($value -eq $TP::MixedAttributeValue) { 'mixed' } elseif ($value -eq $AE::NotSupported) { 'unsupported' } else { $value }
    }
    return $result
}

# An offline independently rendering editor: Windows Forms hosts the installed
# MSHTML engine. Only QEMU input copies and pastes; its DOM supplies observations.
# The request channel may initialize the external fixture, never paste a host
# clipboard representation into the control programmatically.
function Stop-ClipboardPeer {
    $file = Join-Path $QaRoot 'clipboard-peer.pid'
    if (Test-Path $file) {
        $peerId = [int](Get-Content $file)
        $process = Get-CimInstance Win32_Process -Filter "ProcessId = $peerId"
        if ($process -and $process.CommandLine -like '*clipboard-peer.ps1*') { Stop-Process -Id $peerId -Force -ErrorAction SilentlyContinue }
        Remove-Item $file -Force
    }
}

function Start-ClipboardPeer {
    Stop-ClipboardPeer
    Remove-Item "$QaRoot\clipboard-peer-state.json", "$QaRoot\clipboard-peer-request.json" -ErrorAction SilentlyContinue
    $script = @'
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition 'public static class PeerWindow { [System.Runtime.InteropServices.DllImport("user32.dll")] public static extern bool ShowWindow(System.IntPtr window, int command); }'
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$root = 'C:\textloom-qa'
$form = New-Object Windows.Forms.Form
$form.Text = 'Textloom independent HTML clipboard peer (MSHTML)'
# Foreground activation can be denied to jobs; expose the peer for a native click.
$form.TopMost = $true
$form.StartPosition = 'Manual'
$form.Location = New-Object Drawing.Point(80, 40)
$form.Size = New-Object Drawing.Size(1000, 680)
$browser = New-Object Windows.Forms.WebBrowser
$browser.Dock = 'Fill'
$browser.ScriptErrorsSuppressed = $true
$browser.AllowWebBrowserDrop = $false
$browser.WebBrowserShortcutsEnabled = $true
$form.Controls.Add($browser)
$browser.DocumentText = '<!doctype html><html><head><meta http-equiv="X-UA-Compatible" content="IE=edge"><meta charset="utf-8"></head><body contenteditable="true" style="font:18px Segoe UI;background:white;color:black;margin:24px"><p><br></p></body></html>'
$browser.add_Navigating({ if ($_.Url.AbsoluteUri -ne 'about:blank') { $_.Cancel = $true } })
$timer = New-Object Windows.Forms.Timer
$timer.Interval = 250
$script:generation = 0
$timer.add_Tick({
    try {
        if (-not $browser.Document -or -not $browser.Document.Body) { return }
        $requestPath = "$root\clipboard-peer-request.json"
        if (Test-Path $requestPath) {
            $request = Get-Content -Raw $requestPath | ConvertFrom-Json
            Remove-Item $requestPath
            if ($request.action -ne 'fixture') { throw 'unsupported peer request' }
            $browser.Document.Body.InnerHtml = '<p><strong>External caf&#233;</strong> <em>rich</em><br>second line</p><ol start="4"><li>item four</li><li>item five</li></ol>'
            $script:generation = [int]$request.generation
        }
        $elements = @($browser.Document.Body.All | ForEach-Object {
            $style = $_.DomElement.currentStyle
            [ordered]@{ tag = $_.TagName; text = $_.InnerText; html = $_.OuterHtml
                color = $style.color; background = $style.backgroundColor; weight = $style.fontWeight; italic = $style.fontStyle
                href = $_.GetAttribute('href'); start = $_.GetAttribute('start'); value = $_.GetAttribute('value')
                rect = @($_.OffsetRectangle.X, $_.OffsetRectangle.Y, $_.OffsetRectangle.Width, $_.OffsetRectangle.Height) }
        })
        $snapshot = [ordered]@{ generation = $script:generation; text = $browser.Document.Body.InnerText; html = $browser.Document.Body.InnerHtml; elements = $elements
            pid = $PID; engine = (Get-Item "$env:windir\System32\mshtml.dll").VersionInfo.FileVersion }
        $json = $snapshot | ConvertTo-Json -Depth 8 -Compress
        [IO.File]::WriteAllText("$root\clipboard-peer-state.tmp", $json, [Text.UTF8Encoding]::new($false))
        if ([IO.File]::Exists("$root\clipboard-peer-state.json")) {
            [IO.File]::Replace("$root\clipboard-peer-state.tmp", "$root\clipboard-peer-state.json", [NullString]::Value)
        } else {
            [IO.File]::Move("$root\clipboard-peer-state.tmp", "$root\clipboard-peer-state.json")
        }
    } catch {
        [IO.File]::WriteAllText("$root\clipboard-peer-error.txt", ($_ | Out-String))
        $timer.Stop()
    }
})
$form.add_Shown({ [PeerWindow]::ShowWindow($form.Handle, 5) | Out-Null; $form.Activate(); $browser.Focus(); $timer.Start() })
[Windows.Forms.Application]::Run($form)
$timer.Dispose()
$form.Dispose()
'@
    Set-Content -LiteralPath "$QaRoot\clipboard-peer.ps1" -Value $script -Encoding UTF8
    Remove-Item "$QaRoot\clipboard-peer-error.txt" -ErrorAction SilentlyContinue
    $process = Start-Process powershell.exe -ArgumentList '-NoProfile -STA -ExecutionPolicy Bypass -File C:\textloom-qa\clipboard-peer.ps1' -WindowStyle Hidden -PassThru
    Set-Content "$QaRoot\clipboard-peer.pid" $process.Id
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (Test-Path "$QaRoot\clipboard-peer-error.txt") { throw (Get-Content -Raw "$QaRoot\clipboard-peer-error.txt") }
        if (Test-Path "$QaRoot\clipboard-peer-state.json") { return Get-ClipboardPeer }
        if ($process.HasExited) { throw "clipboard peer exited: $($process.ExitCode)" }
        Start-Sleep -Milliseconds 250
    }
    throw 'clipboard peer did not start'
}

function Get-ClipboardPeer {
    if (Test-Path "$QaRoot\clipboard-peer-error.txt") { throw (Get-Content -Raw "$QaRoot\clipboard-peer-error.txt") }
    # Permit atomic replacement while reading an immutable snapshot. PowerShell
    # Get-Content can deny replacement, making a concurrent Move-Item -Force fail.
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        try {
            $stream = [IO.File]::Open("$QaRoot\clipboard-peer-state.json", [IO.FileMode]::Open,
                [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
            $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8)
            try { $value = $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
            break
        } catch [IO.IOException] {
            # ReplaceFile briefly locks the directory entry itself.
            if ($attempt -eq 19) { throw }
            Start-Sleep -Milliseconds 25
        }
    }
    $condition = New-Object System.Windows.Automation.PropertyCondition($AE::ProcessIdProperty, ([int]$value.pid))
    $window = $AE::RootElement.FindAll($Scope::Children, $condition) | Where-Object { $_.Current.Name -like 'Textloom independent HTML*' } | Select-Object -First 1
    $value | Add-Member -NotePropertyName window -NotePropertyValue (Get-Rect $window.Current.BoundingRectangle)
    $value | Add-Member -NotePropertyName origin -NotePropertyValue ([TextloomWin]::ClientOrigin([IntPtr]$window.Current.NativeWindowHandle))
    $foregroundId = 0
    [TextloomWin]::GetWindowThreadProcessId([TextloomWin]::GetForegroundWindow(), [ref]$foregroundId) | Out-Null
    $value | Add-Member -NotePropertyName foreground -NotePropertyValue ($foregroundId -eq $value.pid)
    return $value
}
