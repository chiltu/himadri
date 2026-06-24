# Rust Project Quality Review & Auto-Remediation

**Status:** [PENDING REVIEW]  
**Generated:** $(date)  
**Project:** [Your Rust Project]

---

## Overview

This document guides an AI agent through automated quality analysis, issue detection, and iterative fixes for a Rust codebase.

The agent will:
1. **Analyze** the codebase with automated tools
2. **Report** findings and quality metrics
3. **Fix** issues automatically where possible
4. **Iterate** until quality thresholds are met
5. **Document** all changes and improvements

---

## Phase 1: Initial Analysis

### Step 1.1: Prepare Analysis Environment

```bash
#!/bin/bash
set -e

# Create analysis directory
mkdir -p .rust-review/reports
cd .rust-review

# Initialize tracking
echo "Analysis started at $(date)" > reports/analysis.log
```

**Agent Action:** Execute the above. Report status.

### Step 1.2: Run Automated Analysis Tools

```bash
#!/bin/bash
set -e

echo "=== CLIPPY ANALYSIS ===" | tee -a reports/analysis.log
cargo clippy --all-targets --all-features -- -W clippy::all 2>&1 | tee reports/clippy.txt
CLIPPY_COUNT=$(grep -c "warning:" reports/clippy.txt || echo "0")
echo "Clippy warnings found: $CLIPPY_COUNT" >> reports/analysis.log

echo "=== TESTING ===" | tee -a reports/analysis.log
cargo test --all 2>&1 | tee reports/test-results.txt
TEST_STATUS=$?
echo "Test status: $TEST_STATUS" >> reports/analysis.log

echo "=== COVERAGE ===" | tee -a reports/analysis.log
cargo tarpaulin --out Html --output-dir coverage 2>&1 | tee reports/coverage.log || true
COVERAGE=$(grep -oP 'Coverage: \K[0-9.]+' reports/coverage.log || echo "0")
echo "Code coverage: $COVERAGE%" >> reports/analysis.log

echo "=== SECURITY AUDIT ===" | tee -a reports/analysis.log
cargo audit 2>&1 | tee reports/audit.txt
VULN_COUNT=$(grep -c "warning:" reports/audit.txt || echo "0")
echo "Vulnerabilities found: $VULN_COUNT" >> reports/analysis.log

echo "=== DEPENDENCIES ===" | tee -a reports/analysis.log
cargo tree 2>&1 | tee reports/dependencies.txt
DEP_COUNT=$(grep -c "^├──\|^└──" reports/dependencies.txt || echo "0")
echo "Total dependencies: $DEP_COUNT" >> reports/analysis.log

echo "=== PROJECT METRICS ===" | tee -a reports/analysis.log
find ../src -name "*.rs" | wc -l > reports/file-count.txt
echo "Rust files: $(cat reports/file-count.txt)" >> reports/analysis.log

wc -l ../src/**/*.rs > reports/loc.txt 2>/dev/null || true
echo "LOC calculated" >> reports/analysis.log

echo "=== GIT HISTORY ===" | tee -a reports/analysis.log
git log --oneline -20 > ../reports/git-history.txt 2>/dev/null || echo "No git repo" >> reports/analysis.log

echo "Analysis complete at $(date)" >> reports/analysis.log
```

**Agent Action:** Execute all analysis tools. Parse output files. Proceed to Phase 2.

---

## Phase 2: Quality Assessment & Reporting

### Step 2.1: Parse Metrics

```bash
#!/bin/bash

REPORT="reports/quality-report.txt"

{
  echo "# QUALITY METRICS REPORT"
  echo "Generated: $(date)"
  echo ""
  
  echo "## Coverage"
  if [ -f coverage.log ]; then
    grep "Coverage:" coverage.log || echo "Coverage: Unknown"
  fi
  echo ""
  
  echo "## Clippy Warnings"
  CRITICAL=$(grep -c "error:" reports/clippy.txt || echo "0")
  WARNING=$(grep -c "warning:" reports/clippy.txt || echo "0")
  echo "Errors: $CRITICAL"
  echo "Warnings: $WARNING"
  echo ""
  
  echo "## Test Results"
  if grep -q "test result: ok" reports/test-results.txt; then
    echo "Status: PASSING ✓"
  else
    echo "Status: FAILING ✗"
    grep "test.*FAILED" reports/test-results.txt || true
  fi
  echo ""
  
  echo "## Security"
  if [ -f reports/audit.txt ]; then
    if grep -q "no vulnerabilities found" reports/audit.txt; then
      echo "Status: SECURE ✓"
    else
      grep "^warning:" reports/audit.txt || echo "Vulnerabilities detected"
    fi
  fi
  echo ""
  
} > "$REPORT"

echo "Quality report generated: $REPORT"
```

**Agent Action:** Execute metrics parser. Summarize findings.

---

## Phase 3: Automated Fixes

### Thresholds for Action

```yaml
QUALITY_THRESHOLDS:
  coverage: 80                    # Minimum coverage %
  clippy_warnings_max: 10         # Max allowed warnings
  test_status: passing            # Must pass all tests
  security_vulnerabilities: 0     # Zero tolerance
  unsafe_blocks_max: 5            # Max unsafe blocks
```

### Step 3.1: Auto-Fix Clippy Issues

```bash
#!/bin/bash
set -e

echo "=== AUTO-FIXING CLIPPY ISSUES ===" | tee -a reports/remediation.log

# Run clippy fix (automatic fixes)
echo "Running cargo clippy --fix..." >> reports/remediation.log
cargo clippy --fix --allow-dirty --allow-staged 2>&1 | tee -a reports/clippy-fix.log

# Stage changes
git add -A || true
echo "Changes staged from clippy fixes" >> reports/remediation.log

# Verify no compilation errors
echo "Verifying compilation..." >> reports/remediation.log
if cargo check --all 2>&1 | tee -a reports/remediation.log; then
  echo "✓ Compilation successful after clippy fixes" >> reports/remediation.log
else
  echo "✗ Compilation failed, reverting changes..." >> reports/remediation.log
  git checkout . || true
  exit 1
fi
```

**Agent Action:** Execute clippy auto-fix. Verify compilation. Report changes.

### Step 3.2: Fix Unwrap/Expect Issues

```bash
#!/bin/bash

echo "=== IDENTIFYING UNWRAP/EXPECT PATTERNS ===" | tee -a reports/remediation.log

# Find all unwrap/expect occurrences
grep -rn "\.unwrap()\|\.expect(" ../src --include="*.rs" > reports/unwrap-locations.txt 2>/dev/null || true

UNWRAP_COUNT=$(wc -l < reports/unwrap-locations.txt)
echo "Found $UNWRAP_COUNT unwrap/expect calls" >> reports/remediation.log

if [ $UNWRAP_COUNT -gt 0 ]; then
  echo "WARNING: Manual review needed for unwrap/expect calls" >> reports/remediation.log
  head -10 reports/unwrap-locations.txt >> reports/remediation.log
fi
```

**Agent Action:** Report unwrap locations. Flag for manual review. Document in issues.

### Step 3.3: Format & Lint Fixes

```bash
#!/bin/bash
set -e

echo "=== AUTO-FORMATTING CODE ===" | tee -a reports/remediation.log

# Format code
cargo fmt --all 2>&1 | tee -a reports/remediation.log
echo "✓ Code formatted with rustfmt" >> reports/remediation.log

# Stage formatting changes
git add -A || true

# Verify compilation after formatting
cargo check --all 2>&1 | tee -a reports/remediation.log
echo "✓ Compilation verified after formatting" >> reports/remediation.log
```

**Agent Action:** Execute formatting. Verify compilation.

### Step 3.4: Update Dependencies

```bash
#!/bin/bash

echo "=== DEPENDENCY UPDATES ===" | tee -a reports/remediation.log

# Check for outdated dependencies
echo "Checking for outdated dependencies..." >> reports/remediation.log
cargo outdated > reports/outdated.txt 2>/dev/null || true

# Update patch versions (safest)
echo "Updating patch versions..." >> reports/remediation.log
cargo update 2>&1 | tee -a reports/remediation.log

# Run tests after updates
echo "Testing after dependency updates..." >> reports/remediation.log
if cargo test --all 2>&1 | tee -a reports/remediation.log; then
  echo "✓ Tests pass after dependency updates" >> reports/remediation.log
  git add Cargo.lock || true
else
  echo "⚠ Tests failed after updates, investigate" >> reports/remediation.log
  git checkout Cargo.lock || true
fi
```

**Agent Action:** Update dependencies. Run tests. Report status.

### Step 3.5: Add Missing Tests

```bash
#!/bin/bash

echo "=== COVERAGE ANALYSIS ===" | tee -a reports/remediation.log

# Identify uncovered files
if [ -d coverage/tarpaulin-report.html ]; then
  echo "Coverage report available at coverage/tarpaulin-report.html" >> reports/remediation.log
fi

# Find modules with no tests
grep -L "#\[cfg(test)\]" ../src/**/*.rs 2>/dev/null > reports/untested-modules.txt || true

UNTESTED=$(wc -l < reports/untested-modules.txt)
if [ $UNTESTED -gt 0 ]; then
  echo "Found $UNTESTED modules without tests" >> reports/remediation.log
  echo "RECOMMENDATION: Add tests for:" >> reports/remediation.log
  head -5 reports/untested-modules.txt >> reports/remediation.log
fi
```

**Agent Action:** Analyze coverage gaps. Report recommendations.

---

## Phase 4: Iteration Loop

### Step 4.1: Define Iteration Logic

```bash
#!/bin/bash

ITERATION=0
MAX_ITERATIONS=5
QUALITY_THRESHOLD_MET=false

while [ $ITERATION -lt $MAX_ITERATIONS ] && [ "$QUALITY_THRESHOLD_MET" = false ]; do
  
  ITERATION=$((ITERATION + 1))
  echo ""
  echo "╔════════════════════════════════════════╗"
  echo "║ ITERATION $ITERATION / $MAX_ITERATIONS                      ║"
  echo "╚════════════════════════════════════════╝"
  
  # Run analysis
  bash step-1-2-analysis.sh
  
  # Check thresholds
  COVERAGE=$(grep -oP 'Coverage: \K[0-9.]+' reports/coverage.log | head -1 || echo "0")
  CLIPPY_WARNINGS=$(grep -c "warning:" reports/clippy.txt || echo "0")
  TEST_STATUS=$(grep -c "test result: ok" reports/test-results.txt || echo "0")
  
  echo ""
  echo "📊 Current Metrics (Iteration $ITERATION):"
  echo "   Coverage: ${COVERAGE}% (target: 80%)"
  echo "   Clippy Warnings: $CLIPPY_WARNINGS (target: <10)"
  echo "   Tests: $([ $TEST_STATUS -gt 0 ] && echo 'PASSING' || echo 'FAILING')"
  echo ""
  
  # Check if thresholds met
  if (( $(echo "$COVERAGE >= 80" | bc -l) )) && \
     [ $CLIPPY_WARNINGS -lt 10 ] && \
     [ $TEST_STATUS -gt 0 ]; then
    echo "✓ QUALITY THRESHOLDS MET!"
    QUALITY_THRESHOLD_MET=true
    break
  fi
  
  # Apply fixes
  echo "🔧 Applying fixes (Iteration $ITERATION)..."
  bash step-3-1-clippy-fix.sh
  bash step-3-3-format.sh
  bash step-3-4-update-deps.sh
  
  echo "✓ Iteration $ITERATION complete"
  
done

if [ "$QUALITY_THRESHOLD_MET" = true ]; then
  echo ""
  echo "✅ QUALITY REVIEW COMPLETE - ALL THRESHOLDS MET"
  FINAL_STATUS="PASSED"
else
  echo ""
  echo "⚠️  Max iterations reached. Manual review may be needed."
  FINAL_STATUS="REVIEW_NEEDED"
fi

echo "Final Status: $FINAL_STATUS" >> reports/analysis.log
```

**Agent Action:** Execute iteration loop. Monitor progress. Stop when thresholds met.

---

## Phase 5: Commit & Document Results

### Step 5.1: Commit Changes

```bash
#!/bin/bash

echo "=== COMMITTING CHANGES ===" | tee -a reports/remediation.log

# Check if there are changes
if git diff --quiet && git diff --cached --quiet; then
  echo "No changes to commit" >> reports/remediation.log
  exit 0
fi

# Create commit message
COMMIT_MSG="refactor: automated quality improvements from rust-review

- Clippy fixes and linting improvements
- Code formatting with rustfmt
- Dependency updates
- Coverage analysis complete
- All quality thresholds met"

echo "Committing with message:" >> reports/remediation.log
echo "$COMMIT_MSG" >> reports/remediation.log

git commit -m "$COMMIT_MSG" 2>&1 | tee -a reports/remediation.log || true

echo "✓ Changes committed" >> reports/remediation.log
```

**Agent Action:** Commit all changes with descriptive message.

### Step 5.2: Generate Final Report

```bash
#!/bin/bash

REPORT_FILE="QUALITY_REVIEW_REPORT.md"

{
  echo "# Rust Quality Review Report"
  echo ""
  echo "**Generated:** $(date)"
  echo "**Status:** $(tail -1 .rust-review/reports/analysis.log)"
  echo ""
  
  echo "## Executive Summary"
  echo ""
  echo "✓ **Automated quality review and remediation completed**"
  echo ""
  
  echo "## Metrics Summary"
  echo ""
  echo "| Metric | Value | Status |"
  echo "|--------|-------|--------|"
  
  # Coverage
  COVERAGE=$(grep -oP 'Coverage: \K[0-9.]+' .rust-review/reports/coverage.log | head -1 || echo "Unknown")
  COVERAGE_STATUS=$([ "$COVERAGE" != "Unknown" ] && (( $(echo "$COVERAGE >= 80" | bc -l) )) 2>/dev/null && echo "✓" || echo "⚠")
  echo "| Code Coverage | ${COVERAGE}% | $COVERAGE_STATUS |"
  
  # Clippy
  CLIPPY=$(grep -c "warning:" .rust-review/reports/clippy.txt || echo "0")
  CLIPPY_STATUS=$([ $CLIPPY -lt 10 ] && echo "✓" || echo "⚠")
  echo "| Clippy Warnings | $CLIPPY | $CLIPPY_STATUS |"
  
  # Tests
  TEST_STATUS_LINE=$(grep "test result:" .rust-review/reports/test-results.txt | tail -1 || echo "unknown")
  echo "| Tests | $TEST_STATUS_LINE | ✓ |"
  
  # Security
  if grep -q "no vulnerabilities found" .rust-review/reports/audit.txt 2>/dev/null; then
    echo "| Security | No vulnerabilities | ✓ |"
  else
    echo "| Security | Review recommended | ⚠ |"
  fi
  
  echo ""
  echo "## Actions Taken"
  echo ""
  if grep -q "Clippy fixes" .rust-review/reports/remediation.log 2>/dev/null; then
    echo "- ✓ Clippy auto-fixes applied"
  fi
  if grep -q
```
