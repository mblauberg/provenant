//! Review panel selection for pipeline presets: the shape a review stage
//! selects, and the lenses that fill it.
use super::StageBuilder;
use provenant_routing::{LensPlan, ReviewConfig};
use serde_json::{Value, json};
use std::path::PathBuf;

/// What a review stage asks of the routing review configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShapeQuery<'a> {
    /// Explicit shape (`review_shape`).
    pub shape: Option<&'a str>,
    pub risk: Option<&'a str>,
    pub kind: Option<&'a str>,
    /// Explicit size override (`size`).
    pub size: Option<&'a str>,
    /// Diff reference the change is measured from.
    pub base: Option<&'a str>,
    /// Revision to measure, resolved in `worktree`.
    pub head: Option<&'a str>,
    /// Worktree to measure in; the project directory when absent.
    pub worktree: Option<&'a str>,
}

/// Picks a review shape for a review stage. The host owns routing, so it
/// supplies this from its review configuration.
pub type ShapeSelector = Box<dyn Fn(&ShapeQuery<'_>) -> Result<ShapeHint, String>>;

/// The review shape selected for one review stage. Review shapes are canonical
/// for who reviews; presets compose them, so lens counts and tiers default from here.
#[derive(Clone, Debug, Default)]
pub struct ShapeHint {
    pub name: String,
    pub source: String,
    /// Reviewer pools in shape order, one entry per reviewer.
    pub slots: Vec<String>,
    /// Lens names a shape reviewer group names explicitly, with its pool.
    pub named: Vec<(String, String)>,
    /// Which lenses fill the slots for the stage's change kind.
    pub plan: LensPlan,
    /// Measurement problems; the size was left unknown.
    pub warnings: Vec<String>,
}

/// Build the selector for the routing review configuration. `directory` is
/// where Git runs unless a stage names a worktree.
pub fn review_selector(
    review: ReviewConfig,
    directory: PathBuf,
    task_kind: Option<String>,
) -> ShapeSelector {
    Box::new(move |query| {
        let kind = query.kind.or(task_kind.as_deref());
        let mut warnings = Vec::new();
        let size = match (query.size, query.shape, query.base) {
            (Some(size), _, _) => {
                if !["small", "medium", "large"].contains(&size) {
                    return Err(format!("unknown review size '{size}'"));
                }
                Some(size)
            }
            (None, None, Some(base)) => {
                let cwd = query
                    .worktree
                    .map_or_else(|| directory.clone(), |tree| directory.join(tree));
                let measured =
                    review
                        .size
                        .measure(kind, base, query.head.unwrap_or("HEAD"), &mut |args| {
                            git(&cwd, args)
                        });
                warnings.extend(measured.warning);
                measured.size
            }
            _ => None,
        };
        let selected = review.select_shape(query.shape, query.risk, kind, &[], size)?;
        let mut hint = ShapeHint {
            name: selected.name.to_owned(),
            source: format!(
                "{}; size={}; kind={}; risk={}",
                selected.source,
                size.unwrap_or("unknown"),
                kind.unwrap_or("unknown"),
                query.risk.unwrap_or("unknown")
            ),
            plan: review.lens_plan(kind),
            warnings,
            ..Default::default()
        };
        for group in &selected.shape.reviewers {
            hint.slots
                .extend(std::iter::repeat_n(group.pool.clone(), group.count));
            for lens in &group.lenses {
                hint.named.push((lens.clone(), group.pool.clone()));
            }
        }
        Ok(hint)
    })
}

/// Run Git in `cwd`; an error carries Git's stderr.
fn git(cwd: &std::path::Path, args: &[&str]) -> Result<String, String> {
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    let executable = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
    let mut argv = vec![executable.into_os_string()];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    let output = SystemExecutor::new(1)
        .map_err(|e| e.to_string())?
        .execute(ExecOptions::new(
            &argv,
            cwd,
            std::time::Duration::from_secs(30),
        ))
        .map_err(|e| e.to_string())?;
    if output.termination != Termination::Exit(0) || output.timed_out || output.stdout.truncated {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&output.stderr.bytes).trim()
        ));
    }
    String::from_utf8(output.stdout.bytes).map_err(|e| e.to_string())
}

impl StageBuilder<'_> {
    /// The shape this stage's final parameters select, if shapes are configured.
    pub(super) fn shape_hint(&self) -> Result<Option<ShapeHint>, String> {
        let Some(select) = &self.layers.review else {
            return Ok(None);
        };
        let text = |key: &str| {
            self.parameters
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        let subject = self.parameters.get("subject");
        let subject_type = subject
            .and_then(|subject| subject.get("type"))
            .and_then(Value::as_str);
        let kind = if subject_type == Some("integration") {
            Some("integration")
        } else {
            text("kind")
        };
        // A pinned commit is measured as given. A head or node subject is
        // produced by the graph, so the worktree's current head stands in for it.
        let head = subject
            .and_then(|subject| subject.get("commit"))
            .and_then(Value::as_str)
            .filter(|_| subject_type.is_none_or(|kind| kind == "commit"));
        let hint = select(&ShapeQuery {
            shape: text("review_shape"),
            risk: text("risk_tier"),
            kind,
            size: text("size"),
            base: text("base"),
            head,
            worktree: text("worktree"),
        })?;
        let mut warnings = self.layers.warnings.borrow_mut();
        for warning in &hint.warnings {
            let warning = format!("preset {}: {warning}", self.name);
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        drop(warnings);
        self.layers
            .used_shapes
            .borrow_mut()
            .insert(format!("{} ({})", hint.name, hint.source));
        Ok(Some(hint))
    }

    /// Apply `when` triggers and the selected review shape to a lens list.
    /// An item without a tier takes it from the shape, and only such items count
    /// against the shape's reviewer total; a tier set in the preset is an override.
    /// Lenses the change kind adds sit outside that total. The kind also orders
    /// the lenses and may narrow them (docs), unless the caller supplied the
    /// list, which keeps its own order.
    pub(super) fn select_items(&self, mut list: Vec<Value>) -> Result<Vec<Value>, String> {
        let mut tags: Vec<&str> = self
            .parameters
            .get("risk_tags")
            .and_then(Value::as_array)
            .map(|tags| tags.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if self.parameters.get("risk_tier").and_then(Value::as_str) == Some("terminal") {
            tags.push("terminal");
        }
        let hint = self.shape_hint()?;
        let shape = hint.as_ref();
        let plan = shape.map(|shape| &shape.plan);
        let caller_supplied = self
            .origins
            .get("parameters.lenses")
            .is_some_and(|origin| origin != "bundled");
        let in_list =
            |names: &[String], item: &Value| names.iter().any(|name| item["name"] == *name);
        if let Some(plan) = plan.filter(|plan| !caller_supplied && !plan.order.is_empty()) {
            if plan.exclusive {
                list.retain(|item| {
                    item.get("when")
                        .and_then(Value::as_str)
                        .is_some_and(|when| !when.is_empty())
                        || in_list(&plan.order, item)
                        || in_list(&plan.add, item)
                });
            }
            list.sort_by_key(|item| {
                plan.order
                    .iter()
                    .position(|name| item["name"] == *name)
                    .unwrap_or(plan.order.len())
            });
        }
        let limit = shape.map_or(usize::MAX, |shape| shape.slots.len().max(1));
        let mut governed = 0usize;
        let mut selected = Vec::new();
        for mut item in list {
            let when = item.get("when").and_then(Value::as_str).unwrap_or_default();
            if !when.is_empty() && !tags.contains(&when) {
                self.layers.warnings.borrow_mut().push(format!(
                    "preset {}: omitted lens {}: requires risk tag {when}",
                    self.name, item["name"]
                ));
                continue;
            }
            let tier = item.get("tier").and_then(Value::as_str).unwrap_or_default();
            if tier.is_empty() {
                let name = item["name"].as_str().unwrap_or_default().to_owned();
                if when.is_empty() {
                    if governed < limit {
                        governed += 1;
                    } else if !plan.is_some_and(|plan| in_list(&plan.add, &item)) {
                        self.layers.warnings.borrow_mut().push(format!(
                            "preset {}: omitted lens {name}: review shape has {limit} slots",
                            self.name
                        ));
                        continue;
                    }
                }
                let pool = shape.and_then(|shape| {
                    shape
                        .named
                        .iter()
                        .find(|(lens, _)| name == *lens || name.starts_with(&format!("{lens}-")))
                        .map(|(_, pool)| pool.clone())
                        .or_else(|| {
                            shape
                                .slots
                                .get(governed.saturating_sub(1))
                                .or(shape.slots.last())
                                .cloned()
                        })
                });
                item["tier"] = json!(pool.unwrap_or_else(|| "senior".into()));
            }
            selected.push(item);
        }
        if selected.is_empty() {
            return Err("for_each selected no items".into());
        }
        Ok(selected)
    }
}
