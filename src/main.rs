use rocket::{fs::FileServer, Rocket};

#[macro_use]
extern crate rocket;

#[rocket::main]
async fn main() {
    Rocket::build()
        .mount("/", FileServer::from("./static"))
        .launch()
        .await
        .expect("Rocket failed");
}
