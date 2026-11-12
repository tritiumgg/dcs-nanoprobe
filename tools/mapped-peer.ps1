# The native peer for experiment E24 on Windows, run by `cargo xtask mapped`
# on a windows-ssh target. It does what tools/mapped-peer.py does on Linux:
# maps the file the agent maps inside DCS, reads the eight payload words the
# agent writes under a sequence lock for $a1 seconds, counting torn reads, and
# writes back the last whole value it read (word 16) and its own counter
# (word 17). It prints one JSON object.
#
# xtask sets $a0 to the file's full path and $a1 to the seconds. Words are
# little-endian u64: 0 magic, 1 sequence, 2..9 payload, 16 echo, 17 peer
# sequence. Windows PowerShell 5.1 and .NET Framework only.

$path = $a0
$seconds = [double]$a1
$magic = [UInt64]0x6E706D6170706564
$report = [ordered]@{ path = $path; seconds = $seconds }
$deadline = [DateTime]::UtcNow.AddSeconds(60)
while (-not [IO.File]::Exists($path) -or (New-Object IO.FileInfo($path)).Length -lt 512) {
    if ([DateTime]::UtcNow -gt $deadline) {
        $report.error = 'the file did not appear within 60 s'
        EmitText (ConvertTo-Json $report -Compress)
        exit 1
    }
    Start-Sleep -Milliseconds 50
}
# DCS holds the file open for writing, so open it shared, then map the stream.
$fs = [IO.File]::Open($path, 'Open', 'ReadWrite', 'ReadWrite, Delete')
$map = [IO.MemoryMappedFiles.MemoryMappedFile]::CreateFromFile($fs, [NullString]::Value, 0, 'ReadWrite', $null, 'None', $false)
$view = $map.CreateViewAccessor(0, 512)
function Word([int]$i) { return [UInt64]$view.ReadUInt64($i * 8) }
while ((Word 0) -ne $magic) {
    if ([DateTime]::UtcNow -gt $deadline) {
        $report.error = 'no magic within 60 s'
        EmitText (ConvertTo-Json $report -Compress)
        exit 1
    }
    Start-Sleep -Milliseconds 10
}
$reads = 0; $torn = 0; $busy = 0; $changes = 0; $peer = [UInt64]0
$last = $null; $first = $null
$end = [DateTime]::UtcNow.AddSeconds($seconds)
while ([DateTime]::UtcNow -lt $end) {
    $s1 = Word 1
    if ($s1 % 2 -eq 1) { $busy++; continue }
    $payload = @(2..9 | ForEach-Object { Word $_ })
    $s2 = Word 1
    $reads++
    if ($s1 -ne $s2) { $busy++; continue }
    if (@($payload | Select-Object -Unique).Count -ne 1) { $torn++; continue }
    $value = $payload[0]
    if ($value -ne $last) {
        $changes++
        $last = $value
        if ($null -eq $first) { $first = $value }
        $view.Write(16 * 8, [UInt64]$value)
    }
    $peer++
    $view.Write(17 * 8, [UInt64]$peer)
    Start-Sleep -Milliseconds 1
}
$report.reads = $reads; $report.torn = $torn; $report.retried = $busy
$report.values_seen = $changes; $report.first_value = $first; $report.last_value = $last
$report.peer_sequence = $peer
$view.Dispose(); $map.Dispose(); $fs.Close()
EmitText (ConvertTo-Json $report -Compress)
