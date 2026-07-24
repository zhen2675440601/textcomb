$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$source = Join-Path $repositoryRoot "tests\fixtures\docx-source"
$destination = Join-Path $repositoryRoot "tests\fixtures\e2e-sample.docx"

Add-Type -AssemblyName System.IO.Compression
if (Test-Path -LiteralPath $destination) {
    Remove-Item -LiteralPath $destination -Force
}

$output = [System.IO.File]::Create($destination)
$archive = [System.IO.Compression.ZipArchive]::new(
    $output,
    [System.IO.Compression.ZipArchiveMode]::Create,
    $false
)
try {
    Get-ChildItem -LiteralPath $source -Recurse -File | ForEach-Object {
        $relativePath = $_.FullName.Substring($source.Length + 1).Replace("\", "/")
        $entry = $archive.CreateEntry(
            $relativePath,
            [System.IO.Compression.CompressionLevel]::Optimal
        )
        $entryStream = $entry.Open()
        $input = [System.IO.File]::OpenRead($_.FullName)
        try {
            $input.CopyTo($entryStream)
        }
        finally {
            $input.Dispose()
            $entryStream.Dispose()
        }
    }
}
finally {
    $archive.Dispose()
    $output.Dispose()
}

Write-Output $destination
