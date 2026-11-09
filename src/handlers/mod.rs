pub mod assets;
pub mod deposits;
pub mod orders;
pub mod payments;
pub mod portfolio;
pub mod trades;
pub mod users;
pub mod webhook;

pub fn health() -> &'static str {
    "Ok"
}
