#[cfg(test)]
mod tests {
    use thirtyfour::prelude::ElementQueryable;
    use thirtyfour::By;
    use undetected_chromedriver::chrome;

    #[tokio::test]
    async fn test_headless_detection() {
        let driver = chrome().await.expect("Failed to create Chrome driver");
        driver
            .goto("https://arh.antoinevastel.com/bots/areyouheadless")
            .await
            .expect("Failed to navigate to headless detection page");
        let is_headless = driver.query(By::XPath(r#"//*[@id="res"]/p"#));
        assert_eq!(
            is_headless.first().await.expect("Failed to find headless detection element").text().await.expect("Failed to get text of headless detection element"),
            "You are not Chrome headless"
        );
        driver.quit().await.expect("Failed to quit Chrome driver");
    }
}
