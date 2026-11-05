//! Review panel selection for pipeline presets: the shape a review stage
//! selects, and the lenses that fill it.
use super::StageBuilder;
use provenant_fabric_runtime::graph::{ReviewLens, ReviewSpec};
use provenant_routing::{LensPlan, ReviewConfig, ReviewReviewer, tier_is_set};
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
    /// The host measures the change when the review launches, so expansion
    /// leaves the size unknown and does not read the change.
    pub defer: bool,
}

/// Picks a review shape for a review stage. The host owns routing, so it
/// supplies this from its review configuration.
pub type ShapeSelector = Box<dyn Fn(&ShapeQuery<'_>) -> Result<ShapeHint, String>>;
type GitRunner = dyn Fn(&std::path::Path, &[&str]) -> Result<String, String>;

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
    /// Reviewer groups from the selected review shape.
    pub groups: Vec<ReviewReviewer>,
    /// Which lenses fill the slots for the stage's change kind.
    pub plan: LensPlan,
    /// The review size, measured or given; `None` when unknown.
    pub size: Option<String>,
    /// Measurement problems; the size was left unknown.
    pub warnings: Vec<String>,
    /// Lenses that any changed-path rule or test marker may add at launch.
    pub possible_additions: Vec<String>,
    /// Candidates used when a review supplies no explicit lens list.
    pub default_lenses: Vec<String>,
}

impl ShapeHint {
    /// The `family_group` a lens belongs to when its reviewer group prefers
    /// distinct model families: the group that names the lens, else the group
    /// of the slot it fills. Siblings in a group avoid each other's families as
    /// far as the pool allows; a pool with one family still runs every lens.
    fn family_group(&self, name: &Value, slot: Option<usize>) -> Option<String> {
        let name = name.as_str().unwrap_or_default();
        let named = self.groups.iter().position(|group| {
            group
                .lenses
                .iter()
                .any(|lens| name == lens || name.starts_with(&format!("{lens}-")))
        });
        let index = named.or_else(|| self.group_of_slot(slot?))?;
        self.groups[index]
            .distinct_families
            .then(|| format_family_group(index))
    }

    /// The index of the reviewer group that owns the `slot`th entry of `slots`.
    fn group_of_slot(&self, slot: usize) -> Option<usize> {
        let mut end = 0;
        self.groups.iter().position(|group| {
            end += group.count;
            slot < end
        })
    }
}

/// Build the selector for the routing review configuration. `directory` is
/// where Git runs unless a stage names a worktree.
pub fn review_selector(
    review: ReviewConfig,
    directory: PathBuf,
    task_kind: Option<String>,
) -> ShapeSelector {
    review_selector_with_git(review, directory, task_kind, Box::new(git))
}

fn review_selector_with_git(
    review: ReviewConfig,
    directory: PathBuf,
    task_kind: Option<String>,
    run_git: Box<GitRunner>,
) -> ShapeSelector {
    Box::new(move |query| {
        let kind = query.kind.or(task_kind.as_deref());
        let mut warnings = Vec::new();
        let mut paths = Vec::new();
        let mut test_marker = false;
        let cwd = query
            .worktree
            .map_or_else(|| directory.clone(), |tree| directory.join(tree));
        let base = query
            .base
            .map(str::to_owned)
            .or_else(|| default_base(&run_git, &cwd));
        // A deferred review is measured against its pinned commit by the host;
        // the expansion-time working tree says nothing about it.
        if let Some(base) = base.as_deref().filter(|_| !query.defer) {
            let tip = query.head.unwrap_or("HEAD");
            let range = format!("{base}...{tip}");
            match run_git(
                &cwd,
                &["diff", "--no-renames", "--name-only", "-z", &range, "--"],
            ) {
                Ok(out) => {
                    paths = out
                        .split('\0')
                        .filter(|path| !path.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                Err(error) => warnings.push(format!("cannot read changed paths: {error}")),
            }
            match run_git(
                &cwd,
                &["diff", "--no-ext-diff", "--unified=0", &range, "--"],
            ) {
                Ok(out) => test_marker = provenant_routing::diff_has_test_markers(&out),
                Err(error) => warnings.push(format!("cannot scan test markers: {error}")),
            }
        }
        let size = match (query.size, query.shape, query.base.filter(|_| !query.defer)) {
            (Some(size), _, _) => {
                if !["small", "medium", "large"].contains(&size) {
                    return Err(format!("unknown review size '{size}'"));
                }
                Some(size)
            }
            (None, None, Some(base)) => {
                let measured =
                    review
                        .size
                        .measure(kind, base, query.head.unwrap_or("HEAD"), &mut |args| {
                            run_git(&cwd, args)
                        });
                warnings.extend(measured.warning);
                measured.size
            }
            _ => None,
        };
        let selected = review.select_shape(query.shape, query.risk, kind, &[], size)?;
        let plan = review.lens_plan(kind, &paths, test_marker);
        let lens_reason = plan
            .matched_paths
            .iter()
            .map(|pattern| format!("by_paths:{pattern}"))
            .chain(test_marker.then(|| "test-marker".to_owned()))
            .collect::<Vec<_>>();
        let mut hint = ShapeHint {
            name: selected.name.to_owned(),
            source: format!(
                "{}{}; size={}; kind={}; risk={}",
                selected.source,
                if lens_reason.is_empty() {
                    String::new()
                } else {
                    format!("; lenses={}", lens_reason.join(","))
                },
                size_text(size, query.defer),
                kind.unwrap_or("unknown"),
                query.risk.unwrap_or("unknown")
            ),
            plan,
            size: size.map(str::to_owned),
            warnings,
            groups: selected.shape.reviewers.clone(),
            default_lenses: review
                .lenses
                .order
                .iter()
                .cloned()
                .chain(
                    provenant_routing::RISK_TAG_LENSES
                        .iter()
                        .map(|(_, name)| (*name).to_owned()),
                )
                .collect(),
            possible_additions: review
                .lenses
                .by_paths
                .values()
                .flat_map(|rule| rule.add.iter().cloned())
                .chain(std::iter::once("tests".to_owned()))
                .collect(),
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

fn default_base(run_git: &GitRunner, cwd: &std::path::Path) -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(name) = run_git(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) {
        candidates.push(name.trim().to_owned());
    }
    candidates.extend(["origin/main", "main", "origin/master", "master"].map(str::to_owned));
    candidates.into_iter().find(|name| {
        !name.is_empty()
            && !name.starts_with('-')
            && run_git(
                cwd,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("{name}^{{commit}}"),
                ],
            )
            .is_ok()
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
    pub(super) fn shape_hint(&self, defer: bool) -> Result<Option<ShapeHint>, String> {
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
            defer,
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
        let hint = self.shape_hint(false)?;
        let shape = hint.as_ref();
        let plan = shape.map(|shape| &shape.plan);
        let caller_supplied = self
            .origins
            .get("parameters.lenses")
            .is_some_and(|origin| origin != "bundled");
        if let Some(plan) = plan {
            // A caller's own list keeps its lenses and order; only what the
            // change itself triggers (test paths and markers) is added to it.
            append_plan_additions(
                &mut list,
                if caller_supplied {
                    &plan.change_add
                } else {
                    &plan.add
                },
            );
        }
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
            // A lens the change itself adds (a test path, say) is present whatever
            // its risk-tag condition says; the condition only gates unrequested lenses.
            let triggered = plan.is_some_and(|plan| in_list(&plan.change_add, &item));
            if !when.is_empty() && !tags.contains(&when) && !triggered {
                self.layers.warnings.borrow_mut().push(format!(
                    "preset {}: omitted lens {}: requires risk tag {when}",
                    self.name, item["name"]
                ));
                continue;
            }
            // A route table or list is a tier too; only an empty one defers to the shape.
            let has_tier = item.get("tier").is_some_and(tier_is_set);
            let mut slot = None;
            if !has_tier {
                let name = item["name"].as_str().unwrap_or_default().to_owned();
                if when.is_empty() {
                    if governed < limit {
                        governed += 1;
                        slot = Some(governed - 1);
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
            // Lenses that fill a shape slot join their group's family preference;
            // an explicit tier is an override and stays out of it.
            if let Some(group) = shape.and_then(|shape| shape.family_group(&item["name"], slot)) {
                item["family_group"] = json!(group);
            }
            selected.push(item);
        }
        if selected.is_empty() {
            return Err("for_each selected no items".into());
        }
        Ok(selected)
    }

    /// Whether this stage's review is resolved when the subject is pinned: a
    /// `head` or `node` subject with no explicit size or shape. Explicit commit
    /// subjects, an explicit size or shape, and integration councils expand
    /// statically.
    pub(super) fn defers_review_size(&self) -> bool {
        let subject_type = self
            .parameters
            .get("subject")
            .and_then(|subject| subject.get("type"))
            .and_then(Value::as_str);
        matches!(subject_type, Some("head" | "node")) && self.size_unpinned()
    }

    /// A review whose size and shape the stage leaves to the host.
    fn size_unpinned(&self) -> bool {
        let text = |key: &str| {
            self.parameters
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        self.layers.review.is_some() && text("size").is_none() && text("review_shape").is_none()
    }

    fn review_is_deferred(
        &self,
        entries: &[Value],
        object: &serde_json::Map<String, Value>,
        nodes: &[Value],
    ) -> bool {
        let source = object.get("source");
        if !matches!(entries, [entry] if super::selects_review_lenses(entry))
            || source.and_then(|source| source["type"].as_str()) != Some("node")
        {
            return false;
        }
        // Lenses over a writer review output that does not exist until it runs,
        // whatever commit the preset's own subject names.
        let writes = source
            .and_then(|source| source["node"].as_str())
            .is_some_and(|id| {
                nodes.iter().any(|node| {
                    node["id"] == id && node["access"].as_str() == Some("worktree_write")
                })
            });
        self.size_unpinned() && (writes || self.defers_review_size())
    }

    /// The deferred node that replaces a lens stage: a read-only plan node that
    /// carries the `ReviewSpec` and one lowered lens per candidate. The host
    /// picks the panel when the subject is pinned and builds the lens members
    /// from these. `None` when the stage expands statically.
    #[allow(
        clippy::too_many_arguments,
        reason = "the stage entry shares its bounded state"
    )]
    pub(super) fn deferred_review(
        &mut self,
        id: &str,
        entries: &[Value],
        object: &serde_json::Map<String, Value>,
        deps: &[String],
        outputs: &Value,
        depth: usize,
        nodes: &[Value],
    ) -> Result<Option<Value>, String> {
        // Static and deferred review stages alike record the requested base.
        let is_review = self.layers.review.is_some()
            && matches!(entries, [entry] if super::selects_review_lenses(entry));
        if let Some(base) = self.parameters.get("base").and_then(Value::as_str)
            && is_review
            && !base.is_empty()
        {
            self.layers
                .review_bases
                .borrow_mut()
                .insert(base.to_owned());
        }
        if !self.review_is_deferred(entries, object, nodes) {
            return Ok(None);
        }
        let spec = self.review_spec(object)?;
        let names = self.review_candidates(&spec.lenses)?;
        let items: Vec<Value> = names
            .iter()
            .map(|name| json!({"name": name, "tier": "senior"}))
            .collect();
        let mut lens_entry = entries[0]
            .as_object()
            .cloned()
            .ok_or("stage entries must be tables")?;
        // Templates cover every candidate; selection happens when the head is pinned.
        lens_entry.remove("review_lenses");
        lens_entry.insert("for_each".into(), Value::Array(items));
        let mut leaves = Vec::new();
        self.entry(
            &Value::Object(lens_entry),
            "p1",
            &[],
            &mut leaves,
            depth + 1,
        )?;
        if leaves.len() != names.len() {
            return Err("a deferred review lens stage must lower one node per lens".into());
        }
        let mut read_roots = None;
        let mut templates = serde_json::Map::new();
        for (name, mut leaf) in names.into_iter().zip(leaves) {
            let map = leaf.as_object_mut().ok_or("lens must be a table")?;
            // The host assigns the route, dependencies and read roots.
            let effort = ["route", "routes"]
                .iter()
                .find_map(|key| map.remove(*key))
                .and_then(|route| {
                    route
                        .get("effort")
                        .or_else(|| route.get(0).and_then(|first| first.get("effort")))
                        .cloned()
                });
            if let Some(effort) = effort {
                map.insert("effort".into(), effort);
            }
            map.remove("depends_on");
            map.remove("inputs");
            if let Some(roots) = map.remove("read_roots") {
                read_roots.get_or_insert(roots);
            }
            templates.insert(name, leaf);
        }
        let mut node = json!({
            "id": id,
            "kind": "plan",
            "access": "read_only",
            "depends_on": deps,
            "prompt": "Resolve the review panel for the pinned subject.",
            "review": spec,
            "lens_templates": templates,
        });
        if !deps.is_empty() {
            node["inputs"] = super::selected_inputs(deps, outputs);
        }
        if let Some(roots) = read_roots {
            node["read_roots"] = roots;
        }
        Ok(Some(node))
    }

    /// Every lens a launch can run: the stage's own or the default lenses, and
    /// every lens the change kind, a changed-path rule or a test marker may add.
    fn review_candidates(&self, lenses: &[ReviewLens]) -> Result<Vec<String>, String> {
        let mut names: Vec<String> = lenses.iter().map(|lens| lens.name.clone()).collect();
        if let Some(hint) = self.shape_hint(true)? {
            if names.is_empty() {
                names.extend(hint.default_lenses);
            }
            names.extend(hint.plan.add);
            names.extend(hint.possible_additions);
        }
        names.sort_unstable();
        names.dedup();
        Ok(names)
    }

    /// The `ReviewSpec` this stage hands to launch-time resolution. Only what the
    /// stage states goes in: a size is recorded when the stage overrides it,
    /// and otherwise the host measures it when the review launches.
    pub fn review_spec(
        &self,
        group: &serde_json::Map<String, Value>,
    ) -> Result<ReviewSpec, String> {
        let text = |key: &str| {
            self.parameters
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let subject_type = self
            .parameters
            .get("subject")
            .and_then(|subject| subject.get("type"))
            .and_then(Value::as_str);
        let kind = if subject_type == Some("integration") {
            Some("integration".to_owned())
        } else {
            text("kind")
        };
        let risk_tags = self
            .parameters
            .get("risk_tags")
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut lenses = Vec::new();
        // The bundled list is the default; only a caller's own list is a request.
        let caller_supplied = self
            .origins
            .get("parameters.lenses")
            .is_none_or(|origin| origin != "bundled");
        for lens in self
            .parameters
            .get("lenses")
            .and_then(Value::as_array)
            .filter(|_| caller_supplied)
            .into_iter()
            .flatten()
        {
            let name = lens
                .get("name")
                .and_then(Value::as_str)
                .ok_or("review lens needs a name")?;
            lenses.push(ReviewLens {
                name: name.to_owned(),
                when: lens
                    .get("when")
                    .and_then(Value::as_str)
                    .filter(|when| !when.is_empty())
                    .map(str::to_owned),
                tier: lens
                    .get("tier")
                    .filter(|tier| tier_is_set(tier))
                    .cloned()
                    .unwrap_or(Value::Null),
            });
        }
        // Bound the composite root and every lens a launch can run.
        let names = self.review_candidates(&lenses)?;
        Ok(ReviewSpec {
            shape: text("review_shape"),
            risk_tier: text("risk_tier"),
            kind,
            size: text("size"),
            risk_tags,
            worktree: text("worktree"),
            lenses,
            min_successful: group
                .get("min_successful")
                .map(|value| {
                    let count = if group.get("zero_minimum").and_then(Value::as_str)
                        == Some("all_but_one")
                    {
                        super::interpolate(value, &self.parameters, self.prompt, 1)?
                            .as_u64()
                            .ok_or("min_successful must be a non-negative integer")?
                    } else {
                        self.minimum_successful(value)?
                    };
                    usize::try_from(count).map_err(|_| "min_successful is too large".to_owned())
                })
                .transpose()?,
            max_nodes: names.len().saturating_add(1).max(2),
        })
    }
}

/// The size text when the review measures its change at launch rather than
/// at expansion. Explain output shows it in place of `unknown`.
pub const PENDING_REVIEW_SIZE: &str = "pending (measured at review launch)";

/// How explain output names a review size: the size itself, the pending text
/// when launch measures it, else `unknown`.
fn size_text(size: Option<&str>, pending: bool) -> &str {
    size.unwrap_or(if pending {
        PENDING_REVIEW_SIZE
    } else {
        "unknown"
    })
}

/// The `family_group` for a reviewer group: siblings in it avoid each other's
/// model families where the pool allows.
pub(crate) fn format_family_group(group_index: usize) -> String {
    format!("group-{group_index}")
}

fn append_plan_additions(list: &mut Vec<Value>, additions: &[String]) {
    for name in additions {
        if !list.iter().any(|item| item["name"] == *name) {
            list.push(json!({"name": name}));
        }
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    unused_imports,
    reason = "the custom worktree test driver includes this library without the test harness"
)]
#[path = "panel_tests.rs"]
mod tests;
