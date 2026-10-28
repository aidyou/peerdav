#!/usr/bin/env bash

# PeerDav Release Script
# Updates the project version, commits it, and pushes a v* tag to GitHub.
# The GitHub Actions release workflow builds the installers and creates a draft release.

set -euo pipefail

# Color definitions
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Files that carry the app version.
VERSION_FILES=(
    "package.json"
    "src-tauri/tauri.conf.json"
    "src-tauri/Cargo.toml"
    "src-tauri/Cargo.lock"
)

# Print colored messages
print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1" >&2
}

# Validate version format
validate_version() {
    local version=$1
    # Remove possible 'v' prefix
    version=${version#v}

    # Validate semantic version format (x.y.z)
    if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
        print_error "Invalid version format: $version"
        print_error "Expected format: x.y.z or x.y.z-prerelease (e.g., 1.0.0, 1.0.0-beta.1)"
        exit 1
    fi

    echo "$version"
}

# Get current version from configuration
get_current_version() {
    if [[ -f "src-tauri/tauri.conf.json" ]]; then
        sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1
    else
        echo "unknown"
    fi
}

# Update a JSON file's version field
update_json_version() {
    local file=$1
    local version=$2

    if [[ ! -f "$file" ]]; then
        print_error "File not found: $file"
        exit 1
    fi

    if [[ "$OSTYPE" == "darwin"* ]]; then
        # macOS
        sed -i '' "s/\"version\": *\"[^\"]*\"/\"version\": \"$version\"/" "$file"
    else
        # Linux/Windows
        sed -i "s/\"version\": *\"[^\"]*\"/\"version\": \"$version\"/" "$file"
    fi

    print_success "Updated version in $file to $version"
}

# Update version in Cargo.toml
update_cargo_toml() {
    local version=$1
    local file="src-tauri/Cargo.toml"

    if [[ ! -f "$file" ]]; then
        print_error "File not found: $file"
        exit 1
    fi

    if [[ "$OSTYPE" == "darwin"* ]]; then
        # macOS
        sed -i '' "s/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/version = \"$version\"/" "$file"
    else
        # Linux/Windows
        sed -i "s/^version[[:space:]]*=[[:space:]]*\"[^\"]*\"/version = \"$version\"/" "$file"
    fi

    print_success "Updated version in $file to $version"
}

# Verify that all versions were correctly updated
verify_version_update() {
    local expected_version=$1
    local package_version
    local tauri_version
    local cargo_version

    package_version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' package.json | head -1)
    tauri_version=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)
    cargo_version=$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' src-tauri/Cargo.toml | head -1)

    if [[ "$package_version" != "$expected_version" ]]; then
        print_error "Version mismatch in package.json: expected $expected_version, got $package_version"
        exit 1
    fi

    if [[ "$tauri_version" != "$expected_version" ]]; then
        print_error "Version mismatch in tauri.conf.json: expected $expected_version, got $tauri_version"
        exit 1
    fi

    if [[ "$cargo_version" != "$expected_version" ]]; then
        print_error "Version mismatch in Cargo.toml: expected $expected_version, got $cargo_version"
        exit 1
    fi

    print_success "Version verification passed: $expected_version"
}

# Commit all version-related changes
commit_version_changes() {
    local version=$1

    # Update Cargo.lock after changing Cargo.toml
    print_info "Updating Cargo.lock file..."
    (
        cd src-tauri
        cargo metadata --format-version 1 > /dev/null 2>&1 || cargo generate-lockfile
    )

    # Check for staged, unstaged, or untracked changes in version files
    if [[ -n "$(git status --porcelain -- "${VERSION_FILES[@]}")" ]]; then
        print_info "Committing version changes..."

        git add -- "${VERSION_FILES[@]}"
        git commit -m "chore: bump version to $version" -- "${VERSION_FILES[@]}"

        print_success "Committed version changes"
    else
        print_info "No version changes to commit"
    fi
}

# Get target remote repository. This project is hosted only on GitHub.
get_target_remote() {
    local remote
    local url

    if git remote get-url github >/dev/null 2>&1; then
        url=$(git remote get-url github)
        if [[ "$url" == *"github.com"* ]]; then
            echo "github"
            return 0
        fi
    fi

    if git remote get-url origin >/dev/null 2>&1; then
        url=$(git remote get-url origin)
        if [[ "$url" == *"github.com"* ]]; then
            echo "origin"
            return 0
        fi
    fi

    for remote in $(git remote); do
        url=$(git remote get-url "$remote" 2>/dev/null || echo "")
        if [[ "$url" == *"github.com"* ]]; then
            echo "$remote"
            return 0
        fi
    done

    return 1
}

# Convert a GitHub remote URL to its web URL
get_repo_web_url() {
    local remote=$1
    local url

    url=$(git remote get-url "$remote")

    case "$url" in
        git@github.com:*)
            echo "https://github.com/${url#git@github.com:}" | sed 's/\.git$//'
            ;;
        ssh://git@github.com/*)
            echo "https://github.com/${url#ssh://git@github.com/}" | sed 's/\.git$//'
            ;;
        https://github.com/*)
            echo "${url%.git}"
            ;;
        *)
            echo ""
            ;;
    esac
}

# Create a local tag and push everything to GitHub
create_and_push_tag() {
    local version=$1
    local tag="v$version"
    local remote
    local remote_url
    local remote_tags

    if ! remote=$(get_target_remote); then
        print_error "No GitHub remote found. Add an origin or github remote first."
        exit 1
    fi

    remote_url=$(git remote get-url "$remote")
    print_info "Target remote: $remote ($remote_url)"

    # Handle existing remote tags
    if ! remote_tags=$(git ls-remote --tags "$remote" "refs/tags/$tag"); then
        print_error "Failed to query remote tags on $remote"
        exit 1
    fi

    if [[ -n "$remote_tags" ]]; then
        print_warning "Tag $tag already exists on remote $remote"
        print_warning "Recreating the tag does not delete an existing GitHub release."
        read -p "Do you want to delete and recreate it? (y/N): " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            print_info "Deleting remote tag $tag..."
            if ! git push "$remote" ":refs/tags/$tag"; then
                print_error "Failed to delete remote tag $tag"
                exit 1
            fi

            if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
                git tag -d "$tag"
                print_info "Deleted existing local tag $tag"
            fi
        else
            print_info "Skipping tag creation"
            return 1
        fi
    fi

    # Handle existing local tags
    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        print_warning "Tag $tag already exists locally"
        git tag -d "$tag"
        print_info "Deleted existing local tag $tag"
    fi

    # Push the version commit first
    print_info "Pushing version commit to remote $remote..."
    if git push "$remote" HEAD; then
        print_success "Successfully pushed version commit to $remote"
    else
        print_error "Failed to push version commit to $remote"
        exit 1
    fi

    sleep 2

    # Create tag on current HEAD
    print_info "Creating tag $tag on current HEAD..."
    git tag "$tag" HEAD
    print_success "Created tag $tag"

    # Verify tag points to the correct commit
    local tag_commit
    local head_commit
    tag_commit=$(git rev-list -n 1 "$tag")
    head_commit=$(git rev-parse HEAD)

    if [[ "$tag_commit" != "$head_commit" ]]; then
        print_error "Tag $tag does not point to current HEAD"
        exit 1
    fi

    # Push the tag, which triggers the GitHub release workflow
    print_info "Pushing tag $tag to remote $remote..."
    if git push "$remote" "$tag"; then
        print_success "Successfully pushed tag $tag to $remote"
    else
        print_error "Failed to push tag $tag"
        print_info "You can try pushing manually: git push $remote $tag"
        exit 1
    fi

    local repo_url
    repo_url=$(get_repo_web_url "$remote")
    if [[ -n "$repo_url" ]]; then
        print_info "Check progress at: $repo_url/actions"
    fi
}

# Display usage guide
show_usage() {
    echo "Usage: $0 [version]"
    echo ""
    echo "Examples:"
    echo "  $0                    # Interactive mode"
    echo "  $0 1.0.0             # Release version 1.0.0"
    echo "  $0 v1.0.1-beta.1     # Release pre-release version"
    echo ""
    echo "The script will:"
    echo "  1. Update version in package.json"
    echo "  2. Update version in src-tauri/tauri.conf.json"
    echo "  3. Update version in src-tauri/Cargo.toml"
    echo "  4. Refresh src-tauri/Cargo.lock"
    echo "  5. Commit the version bump"
    echo "  6. Create a Git tag (v{version}) on the current HEAD"
    echo "  7. Push the commit and tag to GitHub"
    echo ""
    echo "Only GitHub remotes are supported. The tag push triggers the release workflow."
}

# Main entry point
main() {
    print_info "PeerDav Release Script"
    print_info "======================="

    # Verify project structure
    if [[ ! -f "src-tauri/tauri.conf.json" ]] || [[ ! -f "src-tauri/Cargo.toml" ]] || [[ ! -f "package.json" ]]; then
        print_error "Please run this script from the project root directory"
        exit 1
    fi

    # Verify git repository
    if ! git rev-parse --git-dir > /dev/null 2>&1; then
        print_error "Not a Git repository"
        exit 1
    fi

    # Pushing HEAD and tagging a detached commit is ambiguous
    if ! git symbolic-ref -q HEAD > /dev/null; then
        print_error "Detached HEAD is not supported; check out a branch before releasing"
        exit 1
    fi

    if ! get_target_remote > /dev/null; then
        print_error "No GitHub remote found. Add an origin or github remote first."
        exit 1
    fi

    local version=""
    local current_version
    current_version=$(get_current_version)

    if [[ -z "$current_version" || "$current_version" == "unknown" ]]; then
        print_error "Unable to read the current version from src-tauri/tauri.conf.json"
        exit 1
    fi

    # Parse arguments
    if [[ $# -eq 0 ]]; then
        # Interactive mode
        print_info "Current version: $current_version"
        echo -n "Enter new version (e.g., 1.0.0 or 1.0.0-beta.1): "
        read -r version

        if [[ -z "$version" ]]; then
            print_error "Version cannot be empty"
            exit 1
        fi
    elif [[ $# -eq 1 ]]; then
        if [[ "$1" == "-h" ]] || [[ "$1" == "--help" ]]; then
            show_usage
            exit 0
        fi
        version=$1
    else
        print_error "Too many arguments"
        show_usage
        exit 1
    fi

    # Sanitize version string
    version=$(validate_version "$version") || exit 1

    if ! command -v cargo > /dev/null 2>&1; then
        print_error "cargo not found; install the Rust toolchain before releasing"
        exit 1
    fi

    # Display summary
    echo ""
    print_info "Release Summary:"
    print_info "  Current version: $current_version"
    print_info "  New version: $version"
    print_info "  Tag: v$version"
    echo ""

    # User confirmation
    read -p "Do you want to proceed with the release? (y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        print_info "Release cancelled."
        exit 0
    fi

    print_info "Starting release process..."

    # Update all version files
    update_json_version "package.json" "$version"
    update_json_version "src-tauri/tauri.conf.json" "$version"
    update_cargo_toml "$version"

    # Validate updates
    verify_version_update "$version"

    # Commit and tag
    commit_version_changes "$version"

    if ! create_and_push_tag "$version"; then
        print_warning "Tag was not pushed; the GitHub release workflow was not triggered."
        exit 0
    fi

    echo ""
    print_success "Release $version completed successfully!"
    print_info "GitHub Actions will now build the installers and create a draft release."
}

main "$@"