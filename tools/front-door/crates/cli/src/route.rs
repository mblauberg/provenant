//! Routing's frozen clap leaves and read-only composition/rendering edge.
use crate::{Adapter, Globals, RenderMode};
use clap::{Args, Subcommand};
use provenant_core::{Roots, WallTime};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{CommandError, CommandResult, Limits, Outcome};
use provenant_routing::{
    BoundedModels, Catalogue, Fallback, ModelId, RouteChoice, RoutingContext, RoutingError,
};
use provenant_supervisor::fs::{Directory, FsError, LocalFs};
use serde::Serialize;
use std::{ffi::OsStr, io, str::FromStr};

#[derive(Debug, Subcommand)]
pub enum Route {
    List(Adapter),
    Resolve(RouteRequest),
}
#[derive(Debug, Args)]
pub struct RouteRequest {
    #[arg(long, conflicts_with = "route")]
    pub model: Option<Model>,
    #[arg(long)]
    pub route: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub confidential: bool,
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Set)]
    pub fallback: Vec<Model>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);
impl FromStr for Model {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ModelId::parse(text).map_err(|_| "model must be ADAPTER/ID")?;
        Ok(Self(text.into()))
    }
}

impl RouteRequest {
    pub fn to_domain(&self) -> Result<provenant_routing::RouteRequest, RoutingError> {
        let choice = match (&self.model, &self.route) {
            (Some(model), None) => RouteChoice::Model(ModelId::parse(&model.0)?),
            (None, Some(name)) => RouteChoice::NamedRoute(name.clone()),
            _ => return Err(RoutingError::InvalidValue),
        };
        let fallback = if self.fallback.is_empty() {
            Fallback::None
        } else {
            Fallback::Models(BoundedModels::new(
                self.fallback
                    .iter()
                    .map(|m| ModelId::parse(&m.0))
                    .collect::<Result<_, _>>()?,
            )?)
        };
        Ok(provenant_routing::RouteRequest {
            choice,
            effort: self.effort.clone(),
            confidential: self.confidential,
            fallback,
        })
    }
}
#[derive(Debug, thiserror::Error)]
enum EdgeError {
    #[error(transparent)]
    Config(#[from] provenant_config::ConfigError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("invalid availability record")]
    Decode(#[from] provenant_records::DecodeError),
    #[error("invalid current time")]
    Time(#[from] provenant_core::TimeError),
}
#[derive(Serialize)]
#[serde(untagged)]
enum Data {
    List(provenant_routing::RouteList),
    Resolved(provenant_records::ResolvedRoute),
}
fn cached(roots: &Roots) -> Result<Vec<AvailabilityRecord>, EdgeError> {
    // Descend descriptor-relatively; no links, missing directories are not writes.
    let open = || -> Result<Directory, FsError> {
        Directory::open_anchor(roots.state.as_path())?
            .open_verified_directory(OsStr::new("routing"))?
            .open_verified_directory(OsStr::new("availability"))
    };
    let directory = match open() {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for reg in REGISTRY {
        let name = format!("{}.json", reg.adapter.id());
        let file = match directory.open_read(OsStr::new(&name)) {
            Ok(file) => file,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error.into()),
        };
        let record: AvailabilityRecord = provenant_records::decode_v1(file, Limits::CONFIG)?;
        provenant_routing::validate_availability(&record, reg.adapter.id())?;
        records.push(record);
    }
    Ok(records)
}
fn execute(command: &Route, roots: &Roots) -> Result<Data, EdgeError> {
    let config = provenant_config::read_with_domains(roots, &LocalFs, &crate::domains::SECTIONS)?
        .domains
        .routing;
    let catalogue = Catalogue::bundled()?;
    let availability = cached(roots)?;
    let timestamp = jiff::Timestamp::now();
    let now = WallTime::from_micros(timestamp.as_microsecond())?;
    let context = RoutingContext {
        catalogue: &catalogue,
        config: &config,
        registry: REGISTRY,
        availability: &availability,
        quota: &[],
        access: &[],
        now,
        host: "local-preview",
    };
    Ok(match command {
        Route::List(query) => Data::List(provenant_routing::list(
            &provenant_routing::RouteListQuery {
                adapter: query.adapter.clone(),
            },
            &context,
        )?),
        Route::Resolve(query) => {
            Data::Resolved(provenant_routing::resolve(&query.to_domain()?, &context)?)
        }
    })
}
pub fn run(command: Route, globals: &Globals, roots: &Roots) -> u8 {
    let name = match command {
        Route::List(_) => "route list",
        Route::Resolve(_) => "route resolve",
    };
    let result = match execute(&command, roots) {
        Ok(data) => CommandResult::new(name, Outcome::Ok, Some(data)),
        Err(error) => {
            let (status, code) = match &error {
                EdgeError::Config(provenant_config::ConfigError::Read { .. })
                | EdgeError::Decode(provenant_records::DecodeError::Read(_)) => {
                    (Outcome::Error, "IO")
                }
                EdgeError::Routing(RoutingError::Availability) | EdgeError::Decode(_) => {
                    (Outcome::Invalid, "AVAILABILITY")
                }
                EdgeError::Routing(_) => (Outcome::Invalid, "INVALID"),
                EdgeError::Config(_) => (Outcome::Invalid, "CONFIG"),
                _ => (Outcome::Error, "IO"),
            };
            let mut result = CommandResult::new(name, status, None);
            result.error = Some(CommandError {
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            result
        }
    };
    let human =
        if let Some(error) = &result.error {
            format!("provenant: {}: {}\n", error.code, error.message)
        } else {
            match &result.data {
                Some(Data::List(list)) => {
                    let mut lines = vec![format!("catalogue {}", list.catalogue_revision)];
                    lines.extend(list.models.iter().map(|m| {
                        format!("{} efforts=[{}]", m.model.as_str(), m.efforts.join(","))
                    }));
                    lines.extend(
                        list.named_routes
                            .iter()
                            .map(|(name, r)| format!("route {name} = {}", r.model.as_str())),
                    );
                    lines.extend(list.pools.iter().map(|(name, members)| {
                        let models = members
                            .iter()
                            .map(|member| {
                                member.effort.as_ref().map_or_else(
                                    || member.model.as_str().to_owned(),
                                    |effort| format!("{}@{effort}", member.model.as_str()),
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("route pool {name} = {models}")
                    }));
                    lines.extend(list.availability.iter().map(|a| {
                        format!(
                            "{} {:?} age={}s",
                            a.adapter,
                            a.reason,
                            a.age_seconds
                                .map_or_else(|| "unknown".into(), |s| s.to_string())
                        )
                    }));
                    format!("{}\n", lines.join("\n"))
                }
                Some(Data::Resolved(route)) => format!(
                    "{}/{} effort={} {:?}\n",
                    route.adapter,
                    route.model,
                    route.effort.as_deref().unwrap_or("unspecified"),
                    route.readiness
                ),
                None => String::new(),
            }
        };
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: &human,
            read_only: true,
        }
    };
    let rendered = if !globals.json && result.error.is_some() {
        crate::render(&result, mode, &mut io::stderr().lock())
    } else {
        crate::render(&result, mode, &mut io::stdout().lock())
    };
    match rendered {
        Ok(()) => crate::exit_status(&result.status),
        Err(crate::RenderError::TooLarge) => {
            let message = "route result exceeds 1 MiB";
            let mut failure = CommandResult::new(name, Outcome::Error, ());
            failure.error = Some(CommandError {
                code: "RESULT_TOO_LARGE".into(),
                message: message.into(),
                cause: None,
                repair: None,
            });
            if globals.json {
                let _ = crate::render(&failure, RenderMode::Json, &mut io::stdout().lock());
            } else {
                let _ = crate::render(
                    &failure,
                    RenderMode::Human {
                        text: "provenant: RESULT_TOO_LARGE: route result exceeds 1 MiB\n",
                        read_only: false,
                    },
                    &mut io::stderr().lock(),
                );
            }
            3
        }
        Err(_) => 3,
    }
}
