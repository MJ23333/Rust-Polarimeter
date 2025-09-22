fn main() {
    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icons/ccme.ico");
        res.compile().unwrap();
    }
}
//Cyclohexene 82.98 30.46 33.47