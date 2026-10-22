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
    $condition = New-Object System.Windows.Automation.PropertyCondition($AE::NameProperty, $Name)
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
