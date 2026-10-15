[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$SetupUrl,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$SetupSha256,
    [Parameter(Mandatory = $true)][string]$BundleUrl,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ManifestSha256,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ReleaseKey,
    [string]$Answers,
    [switch]$Yes
)
$ErrorActionPreference = 'Stop'

function Assert-HttpsUrl([string]$Value, [string]$Label) {
    $uri = $null
    if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref]$uri) -or
        $uri.Scheme -ne 'https' -or $uri.UserInfo -or $uri.Query -or $uri.Fragment) {
        throw "$Label must be an absolute HTTPS URL."
    }
    return $uri
}

$null = Assert-HttpsUrl $SetupUrl 'SetupUrl'
$null = Assert-HttpsUrl $BundleUrl 'BundleUrl'
$tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
$ownedPrefix = 'vesta-setup-bootstrap-'
$work = Join-Path $tempRoot ($ownedPrefix + [Guid]::NewGuid().ToString('N'))
$fullWork = [IO.Path]::GetFullPath($work)
if (-not $fullWork.StartsWith($tempRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or
    [IO.Path]::GetFileName($fullWork) -notlike ($ownedPrefix + '*')) { throw 'Refusing unsafe temporary directory.' }
New-Item -ItemType Directory -Path $fullWork | Out-Null
try {
    $setup = Join-Path $fullWork 'vesta-setup.exe'
    try {
        Add-Type -AssemblyName System.Net.Http
        $handler = [Net.Http.HttpClientHandler]::new()
        $handler.AllowAutoRedirect = $false
        $client = [Net.Http.HttpClient]::new($handler)
        $cts = [Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(180))
        try {
            $uri = [Uri]$SetupUrl
            for ($redirect = 0; $redirect -le 5; $redirect++) {
                $request = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get, $uri)
                $response = $client.SendAsync($request, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cts.Token).GetAwaiter().GetResult()
                if ([int]$response.StatusCode -in @(301, 302, 303, 307, 308)) {
                    if ($redirect -eq 5 -or -not $response.Headers.Location) { throw 'Invalid or excessive redirect.' }
                    $next = [Uri]::new($uri, $response.Headers.Location)
                    if ($next.Scheme -ne 'https' -or $next.UserInfo -or $next.Query -or $next.Fragment) { throw 'Redirect target must use HTTPS.' }
                    $uri = $next
                    $response.Dispose()
                    $request.Dispose()
                    continue
                }
                $response.EnsureSuccessStatusCode()
                if ($response.Content.Headers.ContentLength -gt 67108864) { throw 'Setup binary exceeds 64 MiB.' }
                $source = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                $destination = [IO.File]::Open($setup, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $buffer = New-Object byte[] 65536
                    [long]$total = 0
                    while (($count = $source.ReadAsync($buffer, 0, $buffer.Length, $cts.Token).GetAwaiter().GetResult()) -gt 0) {
                        $total += $count
                        if ($total -gt 67108864) { throw 'Setup binary exceeds 64 MiB.' }
                        $destination.Write($buffer, 0, $count)
                    }
                } finally { $destination.Dispose(); $source.Dispose(); $response.Dispose(); $request.Dispose() }
                break
            }
        } finally { $cts.Dispose(); $client.Dispose(); $handler.Dispose() }
    } catch { throw 'Failed to download setup binary.' }
    $info = Get-Item -LiteralPath $setup
    if ($info.Length -gt 64MB) { throw 'Setup binary exceeds 64 MiB.' }
    $actual = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $SetupSha256.ToLowerInvariant()) { throw 'Setup SHA-256 mismatch.' }
    $argv = @('--bundle-url', $BundleUrl, '--manifest-sha256', $ManifestSha256.ToLowerInvariant(), '--release-key', $ReleaseKey.ToLowerInvariant())
    if ($Answers) { $argv += @('--answers', $Answers) }
    if ($Yes) { $argv += '--yes' }
    & $setup @argv
    if ($LASTEXITCODE -ne 0) { throw 'Setup failed.' }
} finally {
    if (Test-Path -LiteralPath $fullWork) {
        $resolved = [IO.Path]::GetFullPath($fullWork)
        if ($resolved.StartsWith($tempRoot + '\', [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolved) -like ($ownedPrefix + '*')) {
            Remove-Item -LiteralPath $resolved -Recurse -Force
        }
    }
}
