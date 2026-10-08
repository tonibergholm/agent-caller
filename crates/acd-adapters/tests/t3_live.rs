//! Manual pre-merge race gate. Requires a throwaway T3 server and explicit environment.

#[tokio::test]
#[ignore = "live race gate; run only against a throwaway T3 server"]
async fn concurrent_raw_responds_have_one_winner() {
    let _url = std::env::var("T3_LIVE_URL").expect("T3_LIVE_URL");
    let _token = std::env::var("T3_LIVE_TOKEN").expect("T3_LIVE_TOKEN");
    let _thread = std::env::var("T3_LIVE_THREAD").expect("T3_LIVE_THREAD");
    let _request = std::env::var("T3_LIVE_REQUEST").expect("T3_LIVE_REQUEST");
    todo!("implemented by the controller during the live race gate")
}

#[tokio::test]
#[ignore = "live race gate; run only against a throwaway T3 server"]
async fn acd_and_raw_client_race_in_both_orders() {
    let _url = std::env::var("T3_LIVE_URL").expect("T3_LIVE_URL");
    let _token = std::env::var("T3_LIVE_TOKEN").expect("T3_LIVE_TOKEN");
    let _thread = std::env::var("T3_LIVE_THREAD").expect("T3_LIVE_THREAD");
    let _request = std::env::var("T3_LIVE_REQUEST").expect("T3_LIVE_REQUEST");
    todo!("implemented by the controller during the live race gate")
}
