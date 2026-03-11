#[cfg(test)]
mod tests {
    use thirtyfour::prelude::{ElementQueryable, ElementWaitable};
    use thirtyfour::By;
    use undetected_chromedriver::chrome;

    async fn get_score(driver: &thirtyfour::WebDriver) -> Option<f32> {
        driver
            .goto("https://recaptcha-demo.appspot.com/recaptcha-v3-request-scores.php")
            .await
            .expect("Failed to navigate to recaptcha demo page");

        let button = driver
            .query(By::XPath(r#"//*[@id="recaptcha-steps"]"#))
            .first()
            .await
            .expect("Failed to find the button element");
        button
            .wait_until()
            .clickable()
            .await
            .expect("Failed to wait until button is clickable");
        button.click().await.expect("Failed to click the button");
        let response = driver
            .query(By::XPath(r#"//li[@class="step3"]"#))
            .first()
            .await
            .expect("Failed to query response element");
        response
            .wait_until()
            .displayed()
            .await
            .expect("Failed to wait until response is displayed");
        println!(
            "response: {}",
            response.text().await.expect("Failed to get response text")
        );
        let response_text = response.text().await.expect("Failed to get response text");
        let score = response_text
            .lines()
            .find(|line| line.contains("\"score\":"))
            .and_then(|line| {
                let start_index = line.find(':')?;
                let end_index = line.find(',')?;
                line.get(start_index + 1..end_index)
            })
            .and_then(|score_str| score_str.trim().parse::<f32>().ok());
        score
    }

    #[tokio::test]
    async fn recaptcha() {
        let driver = chrome().await.expect("Failed to create Chrome driver");
        let score = get_score(&driver).await;
        assert!(score.unwrap_or(0.0) >= 0.7);
        driver.quit().await.expect("Failed to quit Chrome driver");
    }
}
