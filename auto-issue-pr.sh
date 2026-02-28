#!/bin/bash

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Define clean-tmp function
clean-tmp() {
    echo "Cleaning temporary worktrees in /tmp/..."
    git worktree list | grep "/tmp/" | awk '{print $1}' | while read -r path; do
        git worktree remove "$path" 2>/dev/null || true
    done
    echo "✅ Cleanup complete."
    echo "Tip: Use tmp-worktree <branch-name> to create a new temporary worktree."
}

# Define tmp-worktree function
tmp-worktree() {
    if [ -z "$1" ]; then
        echo "Usage: tmp-worktree <branch-name>"
        echo "Creates a temporary worktree at /tmp/<repo-name>-<branch-name>"
        echo "See also: clean-tmp"
        return 1
    fi
    local branch="$1"
    local dir="/tmp/$(basename "$(pwd)")-$branch"

    # Remove existing directory if it exists
    if [ -d "$dir" ]; then
        echo "Removing existing directory: $dir"
        rm -rf "$dir"
    fi

    # Try to create worktree with existing branch first, then with new branch
    if git worktree add "$dir" "$branch" 2>/dev/null || git worktree add "$dir" -b "$branch"; then
        echo "✅ Worktree created at: $dir"
        cd "$dir"
        return 0
    else
        echo "❌ Failed to create worktree."
        return 1
    fi
}

# Check if we're in a git repository
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    print_error "Not in a git repository. Please run this command from within a git repository."
    exit 1
fi

# Check if gh CLI is installed and authenticated
if ! command -v gh &> /dev/null; then
    print_error "GitHub CLI (gh) is not installed. Please install it first."
    exit 1
fi

if ! gh auth status &> /dev/null; then
    print_error "GitHub CLI is not authenticated. Please run 'gh auth login' first."
    exit 1
fi

# Check if claude command is available
if ! command -v claude &> /dev/null; then
    print_error "Claude CLI is not available. Please ensure it's installed and in your PATH."
    exit 1
fi

print_status "Finding oldest unassigned issue..."

# Get the oldest unassigned issue
ISSUE_JSON=$(gh issue list \
    --state open \
    --assignee "" \
    --limit 1 \
    --json number,title,createdAt,url \
    --jq 'sort_by(.createdAt) | .[0]')

if [ "$ISSUE_JSON" = "null" ] || [ -z "$ISSUE_JSON" ]; then
    print_warning "No unassigned issues found in this repository."
    exit 0
fi

# Extract issue details
ISSUE_NUMBER=$(echo "$ISSUE_JSON" | jq -r '.number')
ISSUE_TITLE=$(echo "$ISSUE_JSON" | jq -r '.title')
ISSUE_URL=$(echo "$ISSUE_JSON" | jq -r '.url')

print_success "Found issue #${ISSUE_NUMBER}: ${ISSUE_TITLE}"
print_status "Issue URL: ${ISSUE_URL}"

# Create branch name from issue
BRANCH_NAME="fix/issue-${ISSUE_NUMBER}"

print_status "Creating temporary worktree with tmp-worktree function..."

# Clean up specific worktree if it exists
WORKTREE_DIR="/tmp/$(basename "$(pwd)")-${BRANCH_NAME}"
if git worktree list | grep -q "$WORKTREE_DIR"; then
    print_warning "Removing existing worktree at $WORKTREE_DIR..."
    git worktree remove "$WORKTREE_DIR" --force 2>/dev/null || true
fi

# Delete the branch if it exists
if git show-ref --verify --quiet "refs/heads/${BRANCH_NAME}"; then
    print_warning "Deleting existing branch ${BRANCH_NAME}..."
    git branch -D "${BRANCH_NAME}" || {
        print_warning "Failed to delete branch, trying to force delete..."
        git update-ref -d "refs/heads/${BRANCH_NAME}" || true
    }
fi

# Use the tmp-worktree function to create and switch to the worktree
if ! tmp-worktree "${BRANCH_NAME}"; then
    print_error "Failed to create worktree. Exiting."
    exit 1
fi

print_success "Created worktree with branch ${BRANCH_NAME}"

print_status "Starting Claude to resolve issue..."

# Start Claude with the hardcoded prompt
claude "I need you to help resolve GitHub issue #${ISSUE_NUMBER}: '${ISSUE_TITLE}'

Issue URL: ${ISSUE_URL}

Please:
1. First, use the gh CLI to get the full issue details and understand what needs to be done
2. Analyze the codebase to understand the context and requirements
3. Implement the necessary changes to resolve the issue
4. Write appropriate tests following TDD principles
5. Run all tests, linting, and formatting checks
6. Create a commit with a clear message referencing the issue
7. Create a draft pull request that addresses the issue

The current working directory is a git worktree specifically created for this issue. Please work systematically and ensure all changes are properly tested before creating the PR.

Start by running: gh issue view ${ISSUE_NUMBER}"

print_success "Claude session completed in tmp-worktree directory"
print_status "Review the changes and merge the PR if satisfied"
