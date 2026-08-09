param(
    [switch]$AllowDirty
)

$ErrorActionPreference = "Stop"

$cargoArguments = @("package", "--list", "--locked")
if ($AllowDirty) {
    $cargoArguments += "--allow-dirty"
}

$packageEntries = @(& cargo @cargoArguments)
if ($LASTEXITCODE -ne 0) {
    throw "cargo package --list --locked failed"
}

$required = @(
    "Cargo.lock",
    "Cargo.toml",
    "LICENSE",
    "README.md",
    "docs/README.md",
    "docs/user/automation.md",
    "docs/user/configuration.md",
    "docs/user/installation.md",
    "docs/user/operations.md",
    "docs/user/permissions.md",
    "docs/user/presentation.md",
    "docs/user/project-context.md",
    "docs/user/sessions.md",
    "src/main.rs"
)

foreach ($path in $required) {
    if ($packageEntries -notcontains $path) {
        throw "required package path is missing: $path"
    }
}

$forbiddenPatterns = @(
    "(^|/)target/",
    "(^|/)\.xana/",
    "(^|/)course/",
    "(^|/)book/",
    "(^|/)\.env($|\.)",
    "(^|/)config\.toml$",
    "(^|/)sessions/",
    "(^|/)artifacts/"
)

foreach ($path in $packageEntries) {
    foreach ($pattern in $forbiddenPatterns) {
        if ($path -match $pattern) {
            throw "forbidden package path matched $pattern`: $path"
        }
    }
}

Write-Output "audited $($packageEntries.Count) package paths"
