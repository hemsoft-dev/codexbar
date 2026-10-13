//! Manual check of the disposable browser (#106): opens Edge or Chrome on a throwaway profile at `https://www.google.com`,
//! lists the cookie names DevTools reports (never values), closes the browser and confirms the profile is gone. Run it
//! on a hidden desktop; it opens a browser window.

fn main() {
    let browser = codexbar_providers::browser_session::find_browser().expect("Edge or Chrome");
    let parent = std::env::temp_dir().join("codexbar-browser-probe");
    std::fs::create_dir_all(&parent).expect("probe folder");
    println!("browser: {}", browser.display());
    {
        let mut session =
            codexbar_providers::browser_session::DisposableBrowser::open(&browser, &parent, "https://www.google.com/")
                .expect("browser and DevTools");
        std::thread::sleep(std::time::Duration::from_secs(8));
        println!("running: {}", session.running());
        let cookies = session.cookies().expect("Storage.getCookies");
        let mut names: Vec<String> = codexbar_providers::gemini_apps::browser_cookies(&cookies)
            .into_iter()
            .map(|cookie| format!("{} ({}, secure={})", cookie.name, cookie.domain, cookie.secure))
            .collect();
        names.sort();
        println!("cookies: {}", names.join(", "));
    }
    let left = std::fs::read_dir(&parent).map(|entries| entries.count()).unwrap_or(0);
    println!("profiles left: {left}");
    let _ = std::fs::remove_dir(&parent);
}
