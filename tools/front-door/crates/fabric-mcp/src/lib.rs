//! Fabric MCP stdio server, tool registration and result rendering.
#![forbid(unsafe_code)]
pub mod protocol;
pub mod render;
pub mod schema;
pub mod stdio;
pub mod tools;

pub(crate) const MAX_WAIT_SECONDS: u64 = provenant_core::WaitTransport::Mcp.maximum_seconds();

#[cfg(test)]
mod coordination_tests;

#[cfg(test)]
pub(crate) mod test_facade {
    macro_rules! local_inbox_owner {
        ($facade:ty) => {
            impl crate::tools::McpFacade for $facade {
                fn inbox_owner(
                    &mut self,
                    input: provenant_fabric_runtime::inbox::InboxInput,
                    host: Option<String>,
                    budget: WaitBudget,
                    cancel: &CancelToken,
                ) -> Result<provenant_fabric_runtime::inbox::InboxResult, ServiceError> {
                    if host.is_some() {
                        return Err(ServiceError::Facade(FacadeError::InvalidInput));
                    }
                    Ok(self.inbox(input, budget, cancel)?)
                }
            }
        };
    }
    pub(crate) use local_inbox_owner;
}
