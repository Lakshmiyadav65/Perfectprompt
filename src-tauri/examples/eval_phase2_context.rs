//! Phase 2 Step 10 evaluation harness — `cargo run --example eval_phase2_context`.
//!
//! Sibling to `eval_pass2.rs`. Runs the deterministic acceptance tests
//! from the Phase 2 brief and prints a verbatim report to stdout.
//!
//! What this harness covers (all deterministic — no LLM call):
//!
//! - **Test 1** — context-bundle correctness for a Project fixture.
//!   Invokes `pipeline::assemble_context_block` directly.
//! - **Test 4** *(rescoped per Step 8 calibration findings)* — verifies
//!   that `context_present` and `effective_threshold` flow correctly
//!   into the router's output. With Mode D's bump set to 0 (pending
//!   Phase 2.5 heuristic re-tune), routing decisions are unchanged
//!   between context-present and context-absent runs. This test proves
//!   the plumbing works; the actual unlock behaviour is deferred.
//! - **Test 5** *(rescoped to routing-decision-only)* — runs the 6
//!   Phase 1 baseline inputs through `router::run` with
//!   `context_present = false` and asserts each lands on the expected
//!   route. Deterministic; no stochastic-LLM-output baseline needed.
//!
//! What this harness does NOT cover (requires a live LLM call and a
//! configured Tauri runtime — run by hand via the hotkey):
//!
//! - **Test 2** — Mode A (stack fill-in) end-to-end. Set up a project
//!   with description "Tauri 2 + React 19 app" and the hotkey input
//!   `add error handling around the API call`. Assert the rewrite
//!   names Rust-flavoured error handling (`Result`, `?`, or `match`) or
//!   Tauri-flavoured (Tauri command / invoke handler), and does NOT
//!   mention `try/catch`. Manual verification.
//! - **Test 3** — Mode B (file naming constrained). With `dashboard`
//!   keyword + a scan that found `Dashboard.tsx`, file naming is
//!   permitted; with `fix the bug` + the same scan, file naming is
//!   forbidden. Manual verification.

use perfectprompt_lib::pipeline::{self};
use perfectprompt_lib::projects::Project;
use perfectprompt_lib::router::{self, RoutingDecision};

fn main() {
    println!("=================================================================");
    println!(" Phase 2 Step 10 — context-aware enhancement acceptance eval");
    println!("=================================================================");
    println!();

    let mut pass = 0;
    let mut fail = 0;

    match run_test_1() {
        Ok(()) => {
            println!("[PASS] Test 1 — context bundle correctness");
            pass += 1;
        }
        Err(reason) => {
            println!("[FAIL] Test 1 — context bundle correctness");
            println!("       reason: {reason}");
            fail += 1;
        }
    }
    println!();

    println!("[NOTE] Test 2 — Mode A (stack fill-in)");
    println!("       Requires live LLM call. Run manually:");
    println!("       1. In ProjectManager, add a project named \"Foo\" with");
    println!("          description \"Tauri 2 + React 19 app\".");
    println!("       2. Set it active.");
    println!("       3. Hotkey input: `add error handling around the API call`.");
    println!("       4. Inspect rewrite — must mention Rust-flavoured error");
    println!("          handling or Tauri command; must NOT mention try/catch.");
    println!();

    println!("[NOTE] Test 3 — Mode B (constrained file naming)");
    println!("       Requires live LLM call + a project with a real scanned");
    println!("       path containing a Dashboard.tsx-like file. Manual eval.");
    println!();

    match run_test_4() {
        Ok(()) => {
            println!("[PASS] Test 4 — context_present + effective_threshold plumbing");
            pass += 1;
        }
        Err(reason) => {
            println!("[FAIL] Test 4 — context_present + effective_threshold plumbing");
            println!("       reason: {reason}");
            fail += 1;
        }
    }
    println!();

    match run_test_5() {
        Ok(()) => {
            println!("[PASS] Test 5 — Phase 1 baseline routing decisions unchanged");
            pass += 1;
        }
        Err(reason) => {
            println!("[FAIL] Test 5 — Phase 1 baseline routing decisions unchanged");
            println!("       reason: {reason}");
            fail += 1;
        }
    }
    println!();

    println!("=================================================================");
    println!(" Deterministic eval: {pass} passed, {fail} failed");
    println!(" (Tests 2 & 3 are manual-LLM — see notes above.)");
    println!("=================================================================");

    if fail > 0 {
        std::process::exit(1);
    }
}

// ─── Fixtures ────────────────────────────────────────────────────────

/// Fixture from the brief: "name 'Foo', description 'Tauri 2 + React 19
/// app', links ['https://github.com/example/foo'], no path."
fn fixture_foo() -> Project {
    Project {
        id: "proj_foo_test".to_string(),
        name: "Foo".to_string(),
        description: "Tauri 2 + React 19 app".to_string(),
        links: vec!["https://github.com/example/foo".to_string()],
        path: None,
        created_at: "0s".to_string(),
        updated_at: "0s".to_string(),
    }
}

// ─── Test 1 — context bundle correctness ─────────────────────────────

fn run_test_1() -> Result<(), String> {
    println!("─── Test 1 ──────────────────────────────────────────────────");
    println!("Fixture: project Foo, description \"Tauri 2 + React 19 app\",");
    println!("         no scan (path is None), no cached fetch.");
    let project = fixture_foo();
    let block = pipeline::assemble_context_block(Some(&project), None, None)
        .ok_or_else(|| "assemble_context_block returned None — expected Some".to_string())?;

    println!();
    println!("Bundle (verbatim):");
    println!("---");
    println!("{block}");
    println!("---");
    println!();

    if !block.starts_with("<context>\nProject: Foo") {
        return Err(format!(
            "expected leading 'Project: Foo' inside <context>, got first 64 chars: {:?}",
            &block.chars().take(64).collect::<String>()
        ));
    }
    let stack_line = block
        .lines()
        .find(|l| l.starts_with("Stack:"))
        .ok_or_else(|| "no Stack: line in bundle".to_string())?;
    if !stack_line.contains("Tauri") {
        return Err(format!("Stack line missing 'Tauri': {stack_line}"));
    }
    if !block.contains("\nDescription:\n") {
        return Err("no Description: section".to_string());
    }
    if !block.contains("Tauri 2 + React 19 app") {
        return Err("Description section is not the verbatim text".to_string());
    }
    if !block.starts_with("<context>\n") || !block.ends_with("\n</context>") {
        return Err("bundle is not wrapped in <context>...</context>".to_string());
    }
    if block.len() > 2000 {
        return Err(format!("bundle exceeds 2000-char cap ({} chars)", block.len()));
    }
    Ok(())
}

// ─── Test 4 — Mode D plumbing (rescoped) ─────────────────────────────

fn run_test_4() -> Result<(), String> {
    println!("─── Test 4 (rescoped) ───────────────────────────────────────");
    println!("Input: `refactor the auth flow` (the brief's canonical Mode D");
    println!("       example). With CONTEXT_THRESHOLD_BUMP = 0, routing is");
    println!("       unchanged between context-absent and context-present");
    println!("       runs — the plumbing carries the signal; the live");
    println!("       unlock effect is deferred to Phase 2.5.");
    let input = "refactor the auth flow";
    let no_ctx = router::run(input, false);
    let ctx = router::run(input, true);

    println!();
    println!(
        "  no-context : route={:?}  effective_threshold={}",
        no_ctx.decision, no_ctx.effective_threshold
    );
    println!(
        "  context    : route={:?}  effective_threshold={}",
        ctx.decision, ctx.effective_threshold
    );
    println!();

    // Plumbing-only assertion: effective_threshold MUST reflect the
    // base DECLINE_THRESHOLD (70) without context, and base + bump
    // with context. The bump value itself is intentionally 0 today;
    // the assertion lives against the formula, not the literal.
    if no_ctx.effective_threshold != 70 {
        return Err(format!(
            "expected effective_threshold=70 without context, got {}",
            no_ctx.effective_threshold
        ));
    }
    // With bump=0, ctx threshold equals base threshold. Once Phase 2.5
    // re-tunes, the harness's expected value updates with the const.
    let expected_with_ctx = 70 + 0;
    if ctx.effective_threshold != expected_with_ctx {
        return Err(format!(
            "expected effective_threshold={} with context, got {}",
            expected_with_ctx, ctx.effective_threshold
        ));
    }
    // Routing must be unchanged at bump=0.
    if std::mem::discriminant(&no_ctx.decision) != std::mem::discriminant(&ctx.decision) {
        return Err(format!(
            "routing diverged between context states (bump=0 should mean no change): {:?} vs {:?}",
            no_ctx.decision, ctx.decision
        ));
    }
    Ok(())
}

// ─── Test 5 — Phase 1 baseline routing (rescoped) ────────────────────

fn run_test_5() -> Result<(), String> {
    println!("─── Test 5 (rescoped to routing-decision-only) ──────────────");
    println!("Asserts the 6 baseline inputs route to their expected");
    println!("category. No LLM call; deterministic. With no project active");
    println!("(context_present = false), routes should match Phase 1.");
    println!();

    // 6 inputs covering Code / Writing / Generic / Decline.
    let cases: &[(&str, ExpectedRoute)] = &[
        (
            "refactor the user service to use async/await instead of promise chains",
            ExpectedRoute::CodeOrBypass,
        ),
        ("refactor the user service", ExpectedRoute::Code),
        ("write a leave mail", ExpectedRoute::Writing),
        ("write a blog post about why we chose postgres", ExpectedRoute::Writing),
        ("summarise this paragraph", ExpectedRoute::Generic),
        ("fix it", ExpectedRoute::Decline),
    ];

    println!(
        "  {:<70} {:>6} {:>4}  {:<10}  {:<10}",
        "input", "ambig", "wc", "expected", "actual"
    );

    let mut all_pass = true;
    for (input, expected) in cases {
        let out = router::run(input, false);
        let wc = input.split_whitespace().count();
        let actual_label = route_label(&out.decision);
        let expected_label = expected.label();
        let pass = expected.matches(&out.decision);
        println!(
            "  {:<70} {:>6} {:>4}  {:<10}  {:<10} {}",
            truncate(input, 70),
            out.ambiguity,
            wc,
            expected_label,
            actual_label,
            if pass { "✓" } else { "✗" }
        );
        if !pass {
            all_pass = false;
        }
    }
    println!();

    if !all_pass {
        return Err("at least one baseline input routed incorrectly".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ExpectedRoute {
    Code,
    CodeOrBypass,
    Writing,
    Generic,
    Decline,
}

impl ExpectedRoute {
    fn matches(&self, d: &RoutingDecision) -> bool {
        match (self, d) {
            (ExpectedRoute::Code, RoutingDecision::Code) => true,
            (ExpectedRoute::CodeOrBypass, RoutingDecision::Code) => true,
            (ExpectedRoute::CodeOrBypass, RoutingDecision::Bypass) => true,
            (ExpectedRoute::Writing, RoutingDecision::Writing) => true,
            (ExpectedRoute::Generic, RoutingDecision::Generic) => true,
            (ExpectedRoute::Decline, RoutingDecision::Decline { .. }) => true,
            _ => false,
        }
    }
    fn label(&self) -> &'static str {
        match self {
            ExpectedRoute::Code => "Code",
            ExpectedRoute::CodeOrBypass => "Code/Bypass",
            ExpectedRoute::Writing => "Writing",
            ExpectedRoute::Generic => "Generic",
            ExpectedRoute::Decline => "Decline",
        }
    }
}

fn route_label(d: &RoutingDecision) -> &'static str {
    match d {
        RoutingDecision::Code => "Code",
        RoutingDecision::Bypass => "Bypass",
        RoutingDecision::Writing => "Writing",
        RoutingDecision::Generic => "Generic",
        RoutingDecision::Decline { .. } => "Decline",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}
