# windows powershell script to release a new version of zinterm
#
# When you run something like .\scripts\release.ps1 v1.2.3 -Push -Remote zinterm_github, the script:
# - Ensures a clean git tree and that the tag doesn’t already exist
# - Updates the version in Cargo.toml / Cargo.lock to 1.2.3
# - Verifies with cargo check and cargo run -- --version
# - Creates a commit and annotated tag v1.2.3
# - If -Push is set, pushes the branch and tag to -Remote
#
# Usage: .\scripts\release.ps1 v1.2.3  # annotated tag
# Usage: .\scripts\release.ps1 v1.2.3 -Push -Remote zinterm_github  # annotated tag and push
# Usage: .\scripts\release.ps1 v1.2.3 -DryRun                       # print actions only, no real modifications

param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidatePattern('^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$')]
    [string] $Tag,               # Required; must look like v1.2.3 or v1.2.3-rc.1

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string] $Remote,            # Required; git remote name to push to (e.g. origin, zinterm_github)

    [switch] $Push,              # Optional; if set -Push, pushes the branch and tag
    [switch] $DryRun             # Optional; if set -DryRun, print actions only, no real modifications
)

$ErrorActionPreference = "Stop"  # Halt on error; prevent partial releases.

# Defines a helper to run git with DryRun support.
function Run-Git {
    param([string[]] $GitArgs)                  # Array of git arguments, e.g. @("add", "Cargo.toml", "Cargo.lock")

    if ($DryRun) {
        Write-Host "git $($GitArgs -join ' ')"  # print the command and return without executing it.
        return
    }

    & git @GitArgs                              # Call external git, splat array into args.
    if ($LASTEXITCODE -ne 0) {                  # Non-zero exit → throw and stop
        throw "git $($GitArgs -join ' ') failed"
    }
}

# Defines a helper to run cargo with DryRun support.
function Run-Cargo {
    param([string[]] $CargoArgs)

    if ($DryRun) {
        Write-Host "cargo $($CargoArgs -join ' ')"
        return
    }

    & cargo @CargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($CargoArgs -join ' ') failed"
    }
}

# Defines a helper to run a command and check the output.
function Run-CheckedOutput {
    param(
        [string] $Expected,  # Expected output string
        [Parameter(ValueFromRemainingArguments = $true)][string[]] $Command  # Remaining args become the command array.
    )

    if ($DryRun) {
        Write-Host "$($Command -join ' ')"                                   # DryRun: print only.
        return
    }

    $rawOutput = & $Command[0] @($Command | Select-Object -Skip 1)           # Run: first element = program, rest = args.
    if ($LASTEXITCODE -ne 0) {                                               # Non-zero exit → throw and stop
        throw "$($Command -join ' ') failed"
    }
    $output = ($rawOutput | Out-String).Trim()                               # Normalize output to trimmed string
    if ($output -ne $Expected) {                                             # Fail if output does not match expected value. (Validate `--version` for new version.)
        throw "Expected '$Expected' but got '$output'."
    }
}

$repoRoot = (& git rev-parse --show-toplevel).Trim()                    # Find repo root path; trim whitespace.
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($repoRoot)) {  # Non-zero exit or empty → throw and stop
    throw "This script must be run inside a git repository."
}

Set-Location $repoRoot                                                  # cd to repo root.

& git diff --quiet --exit-code                                          # Fail if there are unstaged changes to tracked files.
if ($LASTEXITCODE -ne 0) {
    throw "Tracked files have unstaged changes. Commit or stash them before releasing."
}

& git diff --cached --quiet --exit-code                                 # Fail if there are staged changes.
if ($LASTEXITCODE -ne 0) {
    throw "Tracked files have staged changes. Commit or stash them before releasing."
}

$existingTag = (& git tag --list $Tag)                                  # List matching tags; error if the tag already exists
if ($existingTag) {
    throw "Tag '$Tag' already exists."
}

& git remote get-url $Remote | Out-Null                                 # Fail early if the named remote is missing.
if ($LASTEXITCODE -ne 0) {
    throw "Git remote '$Remote' does not exist."
}

$version = $Tag.Substring(1)                                            # Extract version from tag, e.g. v1.2.3 → 1.2.3
$cargoTomlPath = Join-Path $repoRoot "Cargo.toml"                       # Build full paths to Cargo manifests.
$cargoLockPath = Join-Path $repoRoot "Cargo.lock"

$cargoToml = Get-Content -LiteralPath $cargoTomlPath -Raw               # Read whole file as one string
$newCargoToml = [regex]::Replace(
    $cargoToml,
    '(?ms)^(\[package\]\s+.*?^version\s*=\s*")[^"]+(")',
    "`${1}$version`${2}",
    1
)                                                                       # Regex-replace [package].version once
if ($newCargoToml -eq $cargoToml) {                                     # Fail if no change was made.
    throw "Could not update [package].version in Cargo.toml."
}

$cargoLock = Get-Content -LiteralPath $cargoLockPath -Raw
$newCargoLock = [regex]::Replace(
    $cargoLock,
    '(?ms)^(name\s*=\s*"zinterm"\s*)(\r?\n)(version\s*=\s*")[^"]+(")',
    "`${1}`${2}`${3}$version`${4}",
    1
)
if ($newCargoLock -eq $cargoLock) {
    throw "Could not update zinterm version in Cargo.lock."
}

if ($DryRun) {                                                           # DryRun: print only.
    Write-Host "Would set Cargo.toml and Cargo.lock version to $version."
} else {
    # Windows PowerShell 5 uses the active ANSI code page for Set-Content by
    # default, which corrupts non-ASCII comments and makes Cargo reject the
    # manifests as invalid UTF-8. Write explicit UTF-8 without a BOM instead.
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)                   # UTF-8 without BOM
    [System.IO.File]::WriteAllText($cargoTomlPath, $newCargoToml, $utf8NoBom)  # Write both files via .NET
    [System.IO.File]::WriteAllText($cargoLockPath, $newCargoLock, $utf8NoBom)
}

Run-Cargo -CargoArgs @("check", "--locked")                  # Typecheck/build-check; refuse lockfile drift
Run-CheckedOutput -Expected "zinterm $version" -Command @(
    "cargo", "run", "--locked", "--", "--version"
)                                                            # Run app with --version and require exact zinterm <version> output

Run-Git -GitArgs @("add", "Cargo.toml", "Cargo.lock")        # Stage the modified manifests for commit
Run-Git -GitArgs @("commit", "-m", "Release $Tag")           # Commit the release with a message
Run-Git -GitArgs @("tag", "-a", $Tag, "-m", "Release $Tag")  # Annotate the commit with a tag

if ($Push) {  # With -Push: push current branch and tag to $Remote.
    Run-Git -GitArgs @("push", $Remote, "HEAD")
    Run-Git -GitArgs @("push", $Remote, $Tag)
    Write-Host "Released $Tag and pushed branch + tag to '$Remote'."
} else {      # Without -Push: local only; print the manual push commands.
    Write-Host "Created release commit and tag $Tag."
    Write-Host "Push with: git push $Remote HEAD && git push $Remote $Tag"
}
