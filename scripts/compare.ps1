# Times real network copies of the same folders with SMB tools and with yeet.
#
# On the receiving computer (reachable over SSH, so this script can clear it between runs):
#   1. Share a folder over SMB, for example C:\bench shared as "bench".
#   2. Run: yeet-cli receive C:\bench\yeet
#
# On this computer:
#   scripts\compare.ps1 -Share \\pc\bench -RemoteDir C:\bench -Ssh pc -To 192.168.1.20:7878 -Sources 'D:\Games\A'
#
# Each method copies into its own folder, which is emptied after the run so the next one starts
# clean. Results are printed and appended to tools\bench\results\compare.csv.
param(
    [Parameter(Mandatory)] [string] $Share,
    [Parameter(Mandatory)] [string] $RemoteDir,
    [Parameter(Mandatory)] [string] $Ssh,
    [Parameter(Mandatory)] [string] $To,
    [Parameter(Mandatory)] [string[]] $Sources,
    [string[]] $Methods = @('copy-item', 'robocopy', 'robocopy-mt', 'yeet'),
    [string[]] $SshArgs = @(),
    [switch] $NoWarm,
    [string] $Yeet = "$PSScriptRoot\..\target\release\yeet-cli.exe"
)
$ErrorActionPreference = 'Stop'
$csv = "$PSScriptRoot\..\tools\bench\results\compare.csv"

function Copy-With([string] $method, [string] $src) {
    $name = Split-Path $src -Leaf
    switch ($method) {
        # Single-threaded, file by file: close to what Explorer does.
        'copy-item' { Copy-Item $src "$Share\copy-item" -Recurse }
        # robocopy exits with 0-7 on success.
        'robocopy' { robocopy $src "$Share\robocopy\$name" /E /R:2 /W:5 /NFL /NDL /NJH /NJS /NP | Out-Null; if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" } }
        'robocopy-mt' { robocopy $src "$Share\robocopy-mt\$name" /E /MT:16 /R:2 /W:5 /NFL /NDL /NJH /NJS /NP | Out-Null; if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" } }
        'yeet' { $out = & $Yeet send $src --to $To; if ($LASTEXITCODE -ne 0) { throw "yeet failed ($LASTEXITCODE)" }; $out | Select-String 'on wire' | ForEach-Object { "             $_" } }
    }
}

function Invoke-Remote([string] $command) {
    ssh @SshArgs $Ssh $command 2>$null
}

foreach ($src in $Sources) {
    $files = Get-ChildItem $src -Recurse -File
    $bytes = ($files | Measure-Object Length -Sum).Sum
    "== $src  ($($files.Count) files, {0:N1} GB)" -f ($bytes / 1GB)
    # Read everything once so no method pays for a cold cache that the others don't.
    if (-not $NoWarm) { foreach ($f in $files) { $fs = [IO.File]::OpenRead($f.FullName); $fs.CopyTo([IO.Stream]::Null); $fs.Close() } }
    foreach ($method in $Methods) {
        $dir = "$RemoteDir\$method"
        Invoke-Remote "New-Item -ItemType Directory -Force $dir | Out-Null; Remove-Item $dir\* -Recurse -Force"
        $t = [Diagnostics.Stopwatch]::StartNew()
        Copy-With $method $src
        $t.Stop()
        $s = $t.Elapsed.TotalSeconds
        $got = [int](Invoke-Remote "(Get-ChildItem $dir -Recurse -File).Count")
        '{0,-12} {1,8:N1} s  {2,6:N0} Mbit/s  ({3} files arrived)' -f $method, $s, ($bytes * 8 / $s / 1e6), $got
        [pscustomobject]@{ date = (Get-Date -Format s); source = $src; files = $files.Count; bytes = $bytes; method = $method; seconds = [math]::Round($s, 1); arrived = $got } |
            Export-Csv $csv -Append -NoTypeInformation
        Invoke-Remote "Remove-Item $dir\* -Recurse -Force"
    }
}
