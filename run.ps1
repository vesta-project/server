# Quick TCP handshake test for multiplexed port (9090).
# Sends 0x00 prefix required when TCP and HTTP share the same port.

$port = 9090
$tcp = New-Object System.Net.Sockets.TcpClient("localhost", $port)
$stream = $tcp.GetStream()

$msg = '{"type":"handshake","seq":1,"session":"","ts":"2026-01-01T00:00:00Z","version":"1.0.0","client_id":"test-client","capabilities":["sync_v1"]}'
$bytes = [System.Text.Encoding]::UTF8.GetBytes($msg)

$b0 = [byte](($bytes.Length -shr 24) -band 0xFF)
$b1 = [byte](($bytes.Length -shr 16) -band 0xFF)
$b2 = [byte](($bytes.Length -shr 8) -band 0xFF)
$b3 = [byte]($bytes.Length -band 0xFF)

# Single write: 0x00 multiplexer marker + 4-byte length + JSON payload
$frame = New-Object byte[] (1 + 4 + $bytes.Length)
$frame[0] = 0x00
$frame[1] = $b0
$frame[2] = $b1
$frame[3] = $b2
$frame[4] = $b3
[Array]::Copy($bytes, 0, $frame, 5, $bytes.Length)

$stream.Write($frame, 0, $frame.Length)
$stream.Flush()

Start-Sleep -Milliseconds 500

$lenBuf = New-Object byte[] 4
$n = $stream.Read($lenBuf, 0, 4)
if ($n -eq 0) {
    Write-Host "Server closed connection" -ForegroundColor Red
} else {
    $len = ($lenBuf[0] -shl 24) -bor ($lenBuf[1] -shl 16) -bor ($lenBuf[2] -shl 8) -bor $lenBuf[3]
    Write-Host "Response length: $len" -ForegroundColor Cyan
    $data = New-Object byte[] $len
    $read = 0
    while ($read -lt $len) {
        $read += $stream.Read($data, $read, $len - $read)
    }
    $json = [System.Text.Encoding]::UTF8.GetString($data)
    Write-Host $json -ForegroundColor Green
}

$stream.Close()
$tcp.Close()

Write-Host ""
Write-Host "HTTP manifest:" -ForegroundColor Cyan
Invoke-RestMethod -Uri "http://localhost:$port/api/v1/modpack/info" | ConvertTo-Json -Depth 5
