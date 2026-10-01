# Wall-clock comparison of ff, fd and GNU find on one directory tree.
#   ./scripts/bench.ps1 -Root C:\Users\me -Runs 7
# Each command runs once to warm the filesystem cache, then -Runs timed times, with output sent to
# NUL through cmd so terminal rendering is not measured. Prints the median and minimum in ms.
param(
  [Parameter(Mandatory)] [string] $Root,
  [int] $Runs = 7,
  [string] $Only = '',
  [string] $Ff = (Join-Path $PSScriptRoot '..\target\release\ff.exe'),
  [string] $Fd = 'fd',
  [string] $Find = 'C:\Program Files\Git\usr\bin\find.exe'
)

function Measure-Cmd([string] $cmdline) {
  cmd /c "$cmdline > NUL 2>&1" | Out-Null   # warm-up
  $times = 1..$Runs | ForEach-Object {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    cmd /c "$cmdline > NUL 2>&1" | Out-Null
    $sw.Stop()
    $sw.Elapsed.TotalMilliseconds
  } | Sort-Object
  [pscustomobject]@{ Median = [math]::Round($times[[int][math]::Floor($Runs / 2)]); Min = [math]::Round($times[0]) }
}

$ffq = '"' + $Ff + '"'
$findq = '"' + $Find + '"'
$rootq = '"' + $Root + '"'

# Scenarios where all tools see the same files: ignore rules and hidden filtering switched off.
$same = @(
  @{ Name = 'every entry';          Ff = "$ffq -H -I . $rootq";                  Fd = "$Fd -H -I . $rootq";                   Find = "$findq $rootq" },
  @{ Name = 'extension .rs';        Ff = "$ffq -H -I -e rs . $rootq";            Fd = "$Fd -H -I -e rs . $rootq";             Find = "$findq $rootq -name `"*.rs`"" },
  @{ Name = 'name contains config'; Ff = "$ffq -H -I config $rootq";             Fd = "$Fd -H -I config $rootq";              Find = "$findq $rootq -iname `"*config*`"" },
  @{ Name = 'files over 10 MiB';     Ff = "$ffq -H -I -t f -S +10M . $rootq";     Fd = "$Fd -H -I -t f -S +10mi . $rootq";     Find = $null }
)

Write-Host "Root: $Root   runs: $Runs (median / min ms)`n"
Write-Host ('{0,-22} {1,16} {2,16} {3,16}' -f 'scenario', 'ff', 'fd', 'GNU find')
foreach ($s in $same | Where-Object { -not $Only -or $_.Name -like "*$Only*" }) {
  $a = Measure-Cmd $s.Ff
  $b = Measure-Cmd $s.Fd
  # GNU find from Git for Windows stalls for minutes on -size over some trees, so it is skipped there.
  $cText = if ($s.Find) { $c = Measure-Cmd $s.Find; "$($c.Median) / $($c.Min)" } else { 'skipped' }
  Write-Host ('{0,-22} {1,16} {2,16} {3,16}' -f $s.Name, "$($a.Median) / $($a.Min)", "$($b.Median) / $($b.Min)", $cText)
}
