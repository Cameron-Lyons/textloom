# Textloom QA agent: runs in the logged-in desktop session and executes
# PowerShell jobs from winvm.py on the Linux host. It polls the host's job
# server at 10.0.2.2 (QEMU user networking's address for the host), so the
# guest needs no inbound firewall rule. Each request carries the token from
# agent.json, which winvm.py generates per VM.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$config = Get-Content -Raw -LiteralPath (Join-Path $root 'agent.json') | ConvertFrom-Json
$base = "http://10.0.2.2:$($config.port)"
$work = Join-Path $root 'jobs'
New-Item -ItemType Directory -Force $work | Out-Null

$mutex = New-Object Threading.Mutex($false, 'Local\TextloomQaAgent')
if (-not $mutex.WaitOne(0)) { exit }

# Start again at every logon. Registering here also covers a first logon whose
# command did not write the entry, as happened with the LTSC 2024 evaluation.
$command = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$root\agent.ps1`""
Set-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'TextloomQaAgent' -Value $command

function Invoke-Host([string]$Method, [string]$Path, [byte[]]$Body, [int]$Timeout) {
    $request = [Net.HttpWebRequest]::Create("$base$Path")
    $request.Method = $Method
    $request.Timeout = $Timeout
    $request.ReadWriteTimeout = $Timeout
    $request.Proxy = $null
    $request.Headers.Add('X-Token', $config.token)
    if ($null -ne $Body) {
        $request.ContentType = 'application/json'
        $request.ContentLength = $Body.Length
        $stream = $request.GetRequestStream()
        $stream.Write($Body, 0, $Body.Length)
        $stream.Close()
    }
    $response = $request.GetResponse()
    try {
        $reader = New-Object IO.StreamReader($response.GetResponseStream(), [Text.Encoding]::UTF8)
        return @{ Status = [int]$response.StatusCode; Text = $reader.ReadToEnd() }
    } finally {
        $response.Close()
    }
}

# Job scripts get helpers for moving files through the host's share and inbox.
# They keep PowerShell's default error preference: with output redirected,
# Windows PowerShell reports native stderr as errors, which 'Stop' would make fatal.
$prelude = @"
`$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
`$TextloomHost = '$base'
`$TextloomToken = '$($config.token)'
function Get-HostFile([string]`$Name, [string]`$Destination) {
    `$client = New-Object Net.WebClient
    `$client.Headers.Add('X-Token', `$TextloomToken)
    `$client.DownloadFile("`$TextloomHost/share/`$Name", `$Destination)
}
function Send-HostFile([string]`$Path, [string]`$Name) {
    `$client = New-Object Net.WebClient
    `$client.Headers.Add('X-Token', `$TextloomToken)
    `$client.UploadData("`$TextloomHost/inbox/`$Name", 'PUT', [IO.File]::ReadAllBytes(`$Path)) | Out-Null
}

"@

function Invoke-Job($Job) {
    $script = Join-Path $work "$($Job.id).ps1"
    $output = Join-Path $work "$($Job.id).out"
    [IO.File]::WriteAllText($script, $prelude + $Job.script, (New-Object Text.UTF8Encoding($true)))
    $wrapper = "try { & '$script' *>&1 | Out-File -Encoding utf8 -Width 4096 -LiteralPath '$output'; exit [int]`$LASTEXITCODE } " +
        "catch { ('ERROR: ' + `$_.Exception.Message) | Out-File -Append -Encoding utf8 -LiteralPath '$output'; exit 1 }"
    # Windows PowerShell joins argument arrays without quoting, so pass the wrapper encoded.
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($wrapper))
    $arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $encoded"
    $start = @{ FilePath = 'powershell.exe'; ArgumentList = $arguments; WindowStyle = 'Hidden'; PassThru = $true }
    if ($Job.admin) { $start.Verb = 'RunAs' }
    $process = Start-Process @start
    # Wait for this process only: jobs may start GUI programs that keep running.
    $finished = $process.WaitForExit([int]$Job.timeout * 1000)
    if (-not $finished) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    $text = ''
    if (Test-Path -LiteralPath $output) {
        $text = [IO.File]::ReadAllText($output)
    }
    Remove-Item -LiteralPath $script, $output -ErrorAction SilentlyContinue
    $code = if ($finished) { $process.ExitCode } else { -1 }
    return @{ exit = $code; timed_out = -not $finished; output = $text }
}

while ($true) {
    try {
        $response = Invoke-Host 'GET' '/next' $null 40000
        if ($response.Status -ne 200) { continue }
        $job = $response.Text | ConvertFrom-Json
        $result = Invoke-Job $job
        $body = [Text.Encoding]::UTF8.GetBytes(($result | ConvertTo-Json -Compress))
        Invoke-Host 'POST' "/result/$($job.id)" $body 60000 | Out-Null
    } catch {
        Start-Sleep -Seconds 3
    }
}
