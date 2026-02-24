#[cfg(test)]
mod tests {
    use thirtyfour::prelude::ElementQueryable;
    use thirtyfour::By;
    use undetected_chromedriver::Chrome;
    use thirtyfour::WebDriver;

    #[tokio::test]
    #[ignore = "failing to pass on latest nowsecure site"]
    async fn test_cloudflare() {
        let driver: WebDriver = Chrome::new().await;
        driver.bypass_cloudflare("https://nowsecure.nl").await.expect("Failed to bypass Cloudflare");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        println!("{}", driver.source().await.expect("Failed to get driver source"));
        let passed = driver.query(By::XPath("//*[@id=\"success\"]"));
        assert_eq!(passed.first().await.expect("Failed to find passed element").text().await.expect("Failed to get text of passed element"), "you passed!");
        driver.quit().await.expect("Failed to quit Chrome driver");
    }
}