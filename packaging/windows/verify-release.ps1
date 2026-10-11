# Verifies a packaged Windows release before upload: every file vosulay ships
# under its own name carries the product name and version that code signing
# enforces, the ZIP holds the same client DLL as the staged package directory
# the installer is compiled from (the installer itself is not unpacked), and
# with -RequireSignature the signed files have valid Foundation signatures and timestamps.
#
# vosulay.exe is CEF's prebuilt bootstrap with rebranded resources. It is an
# upstream binary, so it is never signed by this project (docs/code-signing.md).
param(
    [Parameter(Mandatory = $true)]
    [string]$Package,
    [Parameter(Mandatory = $true)]
    [string]$Installer,
    [switch]$RequireSignature
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$packagePath = (Resolve-Path -LiteralPath $Package).Path
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$version = (Get-Content -Raw -LiteralPath (Join-Path $packagePath "vosulay-cef.json") |
    ConvertFrom-Json).product_version
if (-not $version) { throw "Package manifest has no product_version" }

function Assert-Metadata([string]$Path, [string]$OriginalFilename) {
    $info = (Get-Item -LiteralPath $Path).VersionInfo
    # Inno Setup pads its version strings with spaces to fixed field widths;
    # Windows shows them trimmed.
    $actual = [ordered]@{
        ProductName      = "$($info.ProductName)".Trim()
        ProductVersion   = "$($info.ProductVersion)".Trim()
        FileVersion      = "$($info.FileVersion)".Trim()
        OriginalFilename = "$($info.OriginalFilename)".Trim()
    }
    $expected = [ordered]@{
        ProductName      = "Vosulay"
        ProductVersion   = $version
        FileVersion      = $version
        OriginalFilename = $OriginalFilename
    }
    foreach ($key in $expected.Keys) {
        # An empty expectation (the installer's OriginalFilename) is not checked.
        if (-not $expected[$key]) { continue }
        if ($actual[$key] -ne $expected[$key]) {
            throw "$Path ${key}: expected '$($expected[$key])', found '$($actual[$key])'"
        }
    }
    Write-Output "metadata ok: $Path"
}

function Assert-Signature([string]$Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($RequireSignature) {
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
            throw "$Path signature is $($signature.Status)"
        }
        if ($null -eq $signature.SignerCertificate -or
            $signature.SignerCertificate.GetNameInfo(
                [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName,
                $false) -ne "SignPath Foundation") {
            throw "$Path signer certificate subject is not SignPath Foundation"
        }
        if ($null -eq $signature.TimeStamperCertificate) {
            throw "$Path signature has no timestamp countersignature"
        }
    }
    Write-Output "signature $($signature.Status): $Path"
}

$client = Join-Path $packagePath "vosulay.dll"
Assert-Metadata (Join-Path $packagePath "vosulay.exe") "vosulay.exe"
Assert-Metadata $client "vosulay.dll"
Assert-Metadata $installerPath ""
Assert-Signature $client
Assert-Signature $installerPath

$zip = Join-Path (Split-Path -Parent $packagePath) "vosulay-$version-windows-x86_64.zip"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
try {
    $entry = $archive.GetEntry("vosulay/vosulay.dll")
    if ($null -eq $entry) { throw "$zip has no vosulay/vosulay.dll" }
    $stream = $entry.Open()
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $zipped = [BitConverter]::ToString($sha.ComputeHash($stream)) -replace "-", ""
    }
    finally {
        $stream.Dispose()
    }
}
finally {
    $archive.Dispose()
}
if ($zipped -ne (Get-FileHash -Algorithm SHA256 -LiteralPath $client).Hash) {
    throw "$zip holds a different vosulay.dll than $packagePath"
}
Write-Output "zip client matches: $zip"
