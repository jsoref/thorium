//! Traits defining shared behavior for interacting with results in the Thorium client
use std::path::Path;
use uuid::Uuid;

use super::GenericClient;
use crate::{
    add_date, add_query, add_query_bool, add_query_list,
    client::Error,
    models::backends::OutputSupport,
    models::{
        Attachment, Cursor, KeySupport, OutputBundle, OutputListLine, OutputMap, OutputRequest,
        OutputResponse, ResultGetParams, ResultListOpts,
    },
    send_build, send_bytes,
};

/// A helper trait containing generic implementations for `ResultsClient`
///
/// The functions are separated to allow for specific docs for each implementation
pub trait ResultsClientHelper: GenericClient {
    /// The underlying type that has the results/outputs (see [`OutputSupport`])
    type OutputSupport: OutputSupport;

    /// Creates an [`Output`] in Thorium
    ///
    /// # Arguments
    ///
    /// * `output_req` - The output request to use to add output from a tool
    async fn create_result_generic(
        &self,
        output_req: OutputRequest<Self::OutputSupport>,
    ) -> Result<OutputResponse, Error> {
        // build url for creating results
        let url = format!(
            "{base}/results/{key}",
            base = self.base_url(),
            key = Self::OutputSupport::key_url(&output_req.key, None)
        );
        // build request
        let req = self
            .client()
            .post(&url)
            .multipart(output_req.to_form().await?)
            .header("authorization", self.token());
        // send this request
        send_build!(self.client(), req, OutputResponse)
    }

    /// Gets results for the `Self::OutputSupport`
    ///
    /// # Arguments
    ///
    /// * `key` - The key to use to access the `Self::OutputSupport`
    /// * `params` - The params to use when getting the results
    async fn get_results_generic<T: AsRef<str>>(
        &self,
        key: T,
        params: &ResultGetParams,
    ) -> Result<OutputMap, Error> {
        // build url for getting a result file
        let url = format!(
            "{base}/results/{key}",
            base = self.base_url(),
            key = key.as_ref(),
        );
        // build our query params
        let mut query = vec![];
        add_query_bool!(query, "hidden", params.hidden);
        add_query_list!(query, "tools[]", params.tools);
        add_query_list!(query, "groups[]", params.groups);
        // build request
        let req = self
            .client()
            .get(&url)
            .header("authorization", self.token())
            .query(&query);
        // send this request and build an output map from the response
        send_build!(self.client(), req, OutputMap)
    }

    /// Downloads a specific result file for the type of `Self::OutputSupport`
    ///
    /// # Arguments
    ///
    /// * `key` - The key to use to access the data to which the results are attached
    /// * `tool` - The tool to that made this result file
    /// * `result_id` - The uuid for this result
    /// * `path` - The path of the result file to download
    async fn download_result_file_generic<T, P>(
        &self,
        key: T,
        tool: &str,
        result_id: &Uuid,
        path: P,
    ) -> Result<Attachment, Error>
    where
        T: AsRef<str>,
        P: AsRef<Path>,
    {
        // build url for downloading results files for the repo
        let url = format!(
            "{base}/result-files/{key}/{tool}/{result_id}",
            base = self.base_url(),
            key = key.as_ref()
        );
        // build our query containing the path to the result file
        let query = vec![("result_file", path.as_ref())];
        // build request
        let req = self
            .client()
            .get(&url)
            .header("authorization", self.token())
            .query(&query);
        // send this request and get the result as bytes
        let data = send_bytes!(self.client(), req)?;
        // build our attachment object from the bytes
        Ok(Attachment { data })
    }

    /// Lists results
    ///
    /// # Arguments
    ///
    /// * `opts` - The options for listing results
    async fn list_results_generic(
        &self,
        opts: ResultListOpts,
    ) -> Result<Cursor<OutputListLine>, Error> {
        // build the url for listing repo results
        let url = format!("{base}/results/", base = self.base_url());
        // get the correct page size if our limit is smaller then our page_size
        let page_size = opts.limit.map_or_else(
            || opts.page_size,
            |limit| std::cmp::min(opts.page_size, limit),
        );
        // build our query params
        let mut query = vec![("limit", page_size.to_string())];
        add_query_list!(query, "groups[]", opts.groups);
        add_date!(query, "start", opts.start);
        add_date!(query, "end", opts.end);
        add_query!(query, "cursor", opts.cursor);
        // get the data for this request and create our cursor
        Cursor::new(
            &url,
            opts.page_size,
            opts.limit,
            self.token(),
            &query,
            self.client(),
        )
        .await
    }

    /// Lists bundled results that meet some search criteria
    ///
    /// # Arguments
    ///
    /// * `opts` - The options for listing result bundles
    async fn list_results_bundle_generic(
        &self,
        opts: ResultListOpts,
    ) -> Result<Cursor<OutputBundle>, Error> {
        // build the url for listing repo results
        let url = format!("{base}/results/bundle/", base = self.base_url());
        // get the correct page size if our limit is smaller then our page_size
        let page_size = opts.limit.map_or_else(
            || opts.page_size,
            |limit| std::cmp::min(opts.page_size, limit),
        );
        // build our query params
        let mut query = vec![("limit", page_size.to_string())];
        add_query_list!(query, "groups[]", opts.groups);
        add_date!(query, "start", opts.start);
        add_date!(query, "end", opts.end);
        add_query!(query, "cursor", opts.cursor);
        // get the data for this request and create our cursor
        Cursor::new(
            &url,
            opts.page_size,
            opts.limit,
            self.token(),
            &query,
            self.client(),
        )
        .await
    }
}

/// Describes a client that is capable of creating and retrieving results for a
/// given Thorium data type
///
/// A client can implement these functions to provide specific docs for its implementation
#[allow(async_fn_in_trait)]
pub trait ResultsClient {
    /// The underlying type that has the results/outputs (see [`OutputSupport`])
    type OutputSupport: OutputSupport;

    async fn create_result(
        &self,
        output_req: OutputRequest<Self::OutputSupport>,
    ) -> Result<OutputResponse, Error>;

    async fn get_results<T: AsRef<str>>(
        &self,
        key: T,
        params: &ResultGetParams,
    ) -> Result<OutputMap, Error>;

    async fn download_result_file<T, P>(
        &self,
        key: T,
        tool: &str,
        result_id: &Uuid,
        path: P,
    ) -> Result<Attachment, Error>
    where
        T: AsRef<str>,
        P: AsRef<Path>;

    async fn list_results(&self, opts: ResultListOpts) -> Result<Cursor<OutputListLine>, Error>;

    async fn list_results_bundle(
        &self,
        opts: ResultListOpts,
    ) -> Result<Cursor<OutputBundle>, Error>;
}
