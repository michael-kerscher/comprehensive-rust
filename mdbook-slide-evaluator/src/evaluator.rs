// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use fantoccini::elements::Element;
use fantoccini::Client;
use log::{debug, warn};
use serde::Serialize;
use strum::Display;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::slides::{Book, Slide};

/// An Evaluator is used to render a book that is a collection of slides
/// and extract information from an element on that page. It further can
/// take a screenshot of this element and store it. A webclient instance is
/// created on creation and dropped once the Evaluator is dropped.
pub struct Evaluator<'a> {
    /// webclient used to render html
    webclient: Client,
    /// selector for the element that is scored
    element_selector: fantoccini::wd::Locator<'a>,
    /// store screenshot in this directory if provided
    screenshot_dir: Option<PathBuf>,
    /// html base uri to the source_dir used as a prefix for each page
    html_base_url: Url,
    /// base directory for all processed files
    source_dir: PathBuf,
    /// if this token is cancelled, the process needs to end gracefully
    cancellation_token: CancellationToken,
    /// the policy applied to the slides
    slide_policy: SlidePolicy,
}

/// element coordinates returned by the browser
#[derive(Debug)]
struct ElementSize {
    /// the width of the element
    width: f64,
    /// the height of the element
    height: f64,
}

impl From<(f64, f64, f64, f64)> for ElementSize {
    fn from(value: (f64, f64, f64, f64)) -> Self {
        Self { width: value.2, height: value.3 }
    }
}

#[derive(Debug)]
/// holds the evaluation result for a slide
pub struct EvaluationResult {
    /// metadata about the slide
    slide: Slide,
    /// all policy violations
    policy_violations: Vec<PolicyViolation>,
}

/// holds all evaluation results for a book
pub struct EvaluationResults {
    /// metadata about the book
    _book: Book,
    /// the collected evaluation results
    results: Vec<EvaluationResult>,
}

#[derive(Serialize)]
struct ExportFormat {
    filename: PathBuf,
    policy_violations: String,
}

impl EvaluationResults {
    /// export the evaluation results to the given csv file, overwrites if
    /// allowed
    pub fn export_csv(
        &self,
        file: &Path,
        overwrite: bool,
        violations_only: bool,
    ) -> anyhow::Result<()> {
        if file.exists() && !overwrite {
            Err(anyhow!(
                "Not allowed to overwrite existing evaluation results at {}",
                file.display()
            ))?;
        };

        let mut csv_writer = csv::Writer::from_path(file)?;
        for result in &self.results {
            if violations_only && result.policy_violations.is_empty() {
                continue;
            }
            csv_writer.serialize(ExportFormat {
                filename: (*result.slide.filename).to_path_buf(),
                policy_violations: result
                    .policy_violations
                    .iter()
                    .map(PolicyViolation::to_string)
                    .collect::<Vec<_>>()
                    .join(";"),
            })?;
        }
        Ok(())
    }

    /// dump the results to stdout
    pub fn export_stdout(&self, violations_only: bool) {
        for result in &self.results {
            if violations_only && result.policy_violations.is_empty() {
                continue;
            }
            println!(
                "{}: [{}]",
                result.slide.filename.display(),
                result
                    .policy_violations
                    .iter()
                    .map(PolicyViolation::to_string)
                    .collect::<Vec<_>>()
                    .join(";"),
            );
        }
    }
}

impl<'a> Evaluator<'_> {
    /// create a new instance with the provided config.
    /// fails if the webclient cannot be created
    pub fn new(
        webclient: Client,
        element_selector: &'a str,
        screenshot_dir: Option<PathBuf>,
        html_base_url: Url,
        source_dir: PathBuf,
        cancellation_token: CancellationToken,
        slide_policy: SlidePolicy,
    ) -> Evaluator<'a> {
        let element_selector = fantoccini::Locator::XPath(element_selector);
        Evaluator {
            webclient,
            element_selector,
            screenshot_dir,
            html_base_url,
            source_dir,
            cancellation_token,
            slide_policy,
        }
    }

    /// navigate the webdriver to the given url.
    /// ensure that html_base_url is set before calling this
    /// after this call the webdriver will see the content at the url
    async fn webdriver_open_url(&self, url: &Url) -> Result<(), anyhow::Error> {
        debug!("open url in webclient: {}", url);
        self.webclient.goto(url.as_str()).await?;
        Ok(())
    }

    /// extract the element coordinates from this element
    async fn get_element_coordinates(
        &self,
        element: &Element,
    ) -> anyhow::Result<ElementSize> {
        let coordinates = Into::<ElementSize>::into(element.rectangle().await?);
        Ok(coordinates)
    }

    /// store the screenshot as png to the given path
    fn store_screenshot(
        &self,
        screenshot: Vec<u8>,
        filename: &Path,
    ) -> anyhow::Result<()> {
        let relative_filename = filename.strip_prefix(&self.source_dir)?;
        let output_filename = self
            .screenshot_dir
            .as_ref()
            .unwrap()
            .join(relative_filename.with_extension("png"));
        debug!("write screenshot to {}", output_filename.to_str().unwrap());

        // create directories if necessary
        let output_dir = output_filename.parent().unwrap();
        if !output_dir.exists() {
            debug!("creating {}", output_dir.to_str().unwrap());
            fs::create_dir_all(output_dir)?;
        }

        let mut file =
            fs::OpenOptions::new().create(true).write(true).open(output_filename)?;

        file.write_all(&screenshot)?;
        Ok(())
    }

    /// evaluates the main element of the page if there is one and returns violations of the policy
    async fn eval_main_element(
        &self,
        slide: &Slide,
    ) -> anyhow::Result<Vec<PolicyViolation>> {
        // get every main content element - should only be one but find_all makes
        // the code easier as we just care about violations of any main element
        let content_elements =
            self.webclient.find_all(self.element_selector).await?;

        let mut violations = vec![];
        for element in content_elements {
            let element_size = self.get_element_coordinates(&element).await?;
            violations.append(&mut self.slide_policy.eval_size(&element_size));
            if self.screenshot_dir.is_some() {
                let screenshot = element.screenshot().await?;
                self.store_screenshot(screenshot, &slide.filename)?;
            }
        }
        Ok(violations)
    }

    /// Determines if the current page has a code example (using ACE) that has
    /// a scrollbar by evaluating if the ace_scrollbar-[hv] element is displayed
    async fn eval_code_example_scrollbar(
        &self,
    ) -> anyhow::Result<Vec<PolicyViolation>> {
        let v_scrollbar_elements_path = fantoccini::Locator::XPath(
            r#"//*[@id="content"]//div[contains(@class, "ace_scrollbar-v")]"#,
        );
        let h_scrollbar_elements_path = fantoccini::Locator::XPath(
            r#"//*[@id="content"]//div[contains(@class, "ace_scrollbar-h")]"#,
        );
        let v_scrollbar_elements =
            self.webclient.find_all(v_scrollbar_elements_path).await?;
        let h_scrollbar_elements =
            self.webclient.find_all(h_scrollbar_elements_path).await?;

        let mut violations = vec![];
        for element in v_scrollbar_elements {
            if element.is_displayed().await? {
                violations.push(PolicyViolation::CodeExampleVScrollbar);
            }
        }
        for element in h_scrollbar_elements {
            if element.is_displayed().await? {
                violations.push(PolicyViolation::CodeExampleHScrollbar);
            }
        }
        Ok(violations)
    }

    /// evaluate a single slide
    pub async fn eval_slide(
        &self,
        slide: &Slide,
    ) -> anyhow::Result<Option<EvaluationResult>> {
        debug!("evaluating {:?}", slide);

        let url = self.html_base_url.join(&slide.filename.display().to_string())?;
        self.webdriver_open_url(&url).await?;

        let mut policy_violations = vec![];
        // evaluate main content element size
        policy_violations.append(&mut self.eval_main_element(slide).await?);
        // evaluate code examples size
        policy_violations.append(&mut self.eval_code_example_scrollbar().await?);

        let result = EvaluationResult { slide: slide.clone(), policy_violations };
        debug!("information about element: {:?}", result);
        Ok(Some(result))
    }

    /// evaluate an entire book
    pub async fn eval_book(&self, book: Book) -> anyhow::Result<EvaluationResults> {
        let mut results = vec![];
        debug!("slide count: {}", book.slides().len());
        for slide in book.slides().iter() {
            if self.cancellation_token.is_cancelled() {
                debug!("received cancel request, return already completed results");
                break;
            }
            let Some(result) = self.eval_slide(slide).await? else {
                warn!("slide with no content - ignore: {:?}", slide);
                continue;
            };
            results.push(result);
        }
        Ok(EvaluationResults { _book: book, results })
    }
}

/// all possible policy violations
#[derive(Debug, Display, Serialize)]
enum PolicyViolation {
    /// violation of the maximum height
    MaxWidth,
    /// violation of the maximum width
    MaxHeight,
    /// code examples should not contain a vertical scrollbar
    CodeExampleVScrollbar,
    /// code examples should not contain a horizontal scrollbar
    CodeExampleHScrollbar,
}

/// the SlidePolicy struct contains all parameters for evaluating a slide
pub struct SlidePolicy {
    /// the maximum allowed width of a slide
    pub max_width: usize,
    /// the maximum allowed height of a slide
    pub max_height: usize,
}

impl SlidePolicy {
    /// evaluate if the width is within the policy
    fn eval_width(&self, element_size: &ElementSize) -> Option<PolicyViolation> {
        if element_size.width as usize > self.max_width {
            return Some(PolicyViolation::MaxWidth);
        }
        return None;
    }

    /// evaluate if the width is within the policy
    fn eval_height(&self, element_size: &ElementSize) -> Option<PolicyViolation> {
        if element_size.height as usize > self.max_height {
            return Some(PolicyViolation::MaxHeight);
        }
        return None;
    }

    /// evaluate all size policies
    fn eval_size(&self, element_size: &ElementSize) -> Vec<PolicyViolation> {
        [self.eval_height(element_size), self.eval_width(element_size)]
            .into_iter()
            .flatten()
            .collect()
    }
}
