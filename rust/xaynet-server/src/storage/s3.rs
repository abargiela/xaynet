use crate::settings::{S3BucketsSettings, S3Settings};
use rusoto_core::{credential::StaticProvider, request::TlsError, HttpClient, RusotoError};
use rusoto_s3::{
    CreateBucketError,
    CreateBucketOutput,
    CreateBucketRequest,
    DeleteObjectsError,
    ListObjectsV2Error,
    PutObjectError,
    PutObjectOutput,
    PutObjectRequest,
    S3Client,
    StreamingBody,
    S3,
};
use std::sync::Arc;
use thiserror::Error;

use xaynet_core::mask::Model;

type S3Result<T> = Result<T, S3Error>;

#[derive(Debug, Error)]
pub enum S3Error {
    #[error("upload error: {0}")]
    Upload(#[from] RusotoError<PutObjectError>),
    #[error("create bucket error: {0}")]
    CreateBucket(#[from] RusotoError<CreateBucketError>),
    #[error("list objects error: {0}")]
    ListObjects(#[from] RusotoError<ListObjectsV2Error>),
    #[error("delete objects error: {0}")]
    DeleteObjects(#[from] RusotoError<DeleteObjectsError>),
    #[error("serialization failed")]
    Serialization(#[from] bincode::Error),
    #[error("empty response error")]
    EmptyResponse,
    #[error(transparent)]
    HttpClient(#[from] TlsError),
}

#[derive(Clone)]
pub struct Client {
    buckets: Arc<S3BucketsSettings>,
    s3_client: S3Client,
}

#[cfg(test)]
impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("buckets", &self.buckets)
            .finish()
    }
}

impl Client {
    /// Creates a new S3 client. The client creates and maintains one bucket for storing global models.
    ///
    /// To connect to AWS-compatible services such as Minio, you need to specify a custom region.
    /// ```
    /// use rusoto_core::Region;
    /// use xaynet_server::{
    ///     settings::{S3BucketsSettings, S3Settings},
    ///     storage::s3::Client,
    /// };
    ///
    /// let region = Region::Custom {
    ///     name: String::from("minio"),
    ///     endpoint: String::from("http://127.0.0.1:9000"), // URL of minio
    /// };
    ///
    /// let s3_settings = S3Settings {
    ///     region,
    ///     access_key: String::from("minio"),
    ///     secret_access_key: String::from("minio123"),
    ///     buckets: S3BucketsSettings {
    ///         global_models: String::from("global-models"),
    ///     },
    /// };
    ///
    /// let store = Client::new(s3_settings);
    /// ```
    pub fn new(settings: S3Settings) -> S3Result<Self> {
        let credentials_provider =
            StaticProvider::new_minimal(settings.access_key, settings.secret_access_key);

        let dispatcher = HttpClient::new()?;
        Ok(Self {
            buckets: Arc::new(settings.buckets),
            s3_client: S3Client::new_with(dispatcher, credentials_provider, settings.region),
        })
    }

    /// Uploads a global model.
    pub async fn upload_global_model(&self, key: &str, global_model: &Model) -> S3Result<()> {
        debug!("store global model: {}", key);
        let data = bincode::serialize(global_model)?;
        self.upload(&self.buckets.global_models, key, data)
            .await
            .map_err(From::from)
            .map(|_| ())
    }

    /// Creates the `global_models` bucket.
    pub async fn create_global_models_bucket(&self) -> S3Result<()> {
        debug!("create global-models bucket");
        match self.create_bucket("global-models").await {
            Ok(_)
            | Err(RusotoError::Service(CreateBucketError::BucketAlreadyExists(_)))
            | Err(RusotoError::Service(CreateBucketError::BucketAlreadyOwnedByYou(_))) => Ok(()),
            Err(err) => Err(S3Error::from(err)),
        }
    }

    // Uploads an object to the given bucket.
    async fn upload(
        &self,
        bucket: &str,
        key: &str,
        data: Vec<u8>,
    ) -> Result<PutObjectOutput, RusotoError<PutObjectError>> {
        let req = PutObjectRequest {
            bucket: bucket.to_string(),
            key: key.to_string(),
            body: Some(StreamingBody::from(data)),
            ..Default::default()
        };
        self.s3_client.put_object(req).await
    }

    // Creates a new bucket with the given bucket name.
    async fn create_bucket(
        &self,
        bucket: &str,
    ) -> Result<CreateBucketOutput, RusotoError<CreateBucketError>> {
        let req = CreateBucketRequest {
            bucket: bucket.to_string(),
            ..Default::default()
        };
        self.s3_client.create_bucket(req).await
    }
}

#[cfg(test)]
pub(in crate) mod tests {
    use super::*;
    use crate::storage::tests::create_global_model;
    use rusoto_core::Region;
    use rusoto_s3::{
        Delete,
        DeleteObjectsOutput,
        DeleteObjectsRequest,
        GetObjectOutput,
        GetObjectRequest,
        ListObjectsV2Output,
        ListObjectsV2Request,
        ObjectIdentifier,
    };
    use serial_test::serial;
    use tokio::io::AsyncReadExt;
    use xaynet_core::{common::RoundSeed, crypto::ByteObject};

    impl Client {
        // Deletes all objects in a bucket.
        pub async fn clear_bucket(&self, bucket: &str) -> S3Result<()> {
            let mut continuation_token: Option<String> = None;

            loop {
                let list_obj_resp = self.list_objects(bucket, continuation_token).await?;

                if let Some(identifiers) = Self::unpack_object_identifier(&list_obj_resp) {
                    self.delete_objects(bucket, identifiers).await?;
                } else {
                    break;
                }

                // check if more objects exist
                continuation_token = Self::unpack_next_continuation_token(&list_obj_resp);
                if continuation_token.is_none() {
                    break;
                }
            }
            Ok(())
        }

        // Download a global model.
        pub async fn download_global_model(&self, key: &str) -> Model {
            debug!("get global model {:?}", key);
            let object = self.download_object(&self.buckets.global_models, key).await;
            let content = Self::unpack_object(object).await.expect("unpack error");
            bincode::deserialize(&content).expect("deserialization error")
        }

        // Unpacks the object identifier/keys of a [`ListObjectsV2Output`] response.
        fn unpack_object_identifier(
            list_obj_resp: &ListObjectsV2Output,
        ) -> Option<Vec<ObjectIdentifier>> {
            if let Some(objects) = &list_obj_resp.contents {
                let keys = objects
                    .iter()
                    .filter_map(|obj| obj.key.clone())
                    .map(|key| ObjectIdentifier {
                        key,
                        ..Default::default()
                    })
                    .collect();
                Some(keys)
            } else {
                None
            }
        }

        // Deletes the objects of the given bucket.
        async fn delete_objects(
            &self,
            bucket: &str,
            identifiers: Vec<ObjectIdentifier>,
        ) -> Result<DeleteObjectsOutput, RusotoError<DeleteObjectsError>> {
            let req = DeleteObjectsRequest {
                bucket: bucket.to_string(),
                delete: Delete {
                    objects: identifiers,
                    ..Default::default()
                },
                ..Default::default()
            };

            self.s3_client.delete_objects(req).await.map_err(From::from)
        }

        // Returns all object keys for the given bucket.
        async fn list_objects(
            &self,
            bucket: &str,
            continuation_token: Option<String>,
        ) -> Result<ListObjectsV2Output, RusotoError<ListObjectsV2Error>> {
            let req = ListObjectsV2Request {
                bucket: bucket.to_string(),
                continuation_token,
                // the AWS response is limited to 1000 keys max.
                // https://docs.aws.amazon.com/AWSJavaScriptSDK/latest/AWS/S3.html#listObjectsV2-property
                // However, Minio could return more.
                max_keys: Some(1000),
                ..Default::default()
            };

            self.s3_client
                .list_objects_v2(req)
                .await
                .map_err(From::from)
        }

        // Unpacks the next_continuation_token of the [`ListObjectsV2Output`] response.
        fn unpack_next_continuation_token(list_obj_resp: &ListObjectsV2Output) -> Option<String> {
            // https://docs.aws.amazon.com/AmazonS3/latest/dev/ListingObjectKeysUsingJava.html
            if let Some(is_truncated) = list_obj_resp.is_truncated {
                if is_truncated {
                    list_obj_resp.next_continuation_token.clone()
                } else {
                    None
                }
            } else {
                None
            }
        }

        // Get the content of the given object.
        async fn unpack_object(object: GetObjectOutput) -> S3Result<Vec<u8>> {
            let mut content = Vec::new();
            object
                .body
                .ok_or(S3Error::EmptyResponse)?
                .into_async_read()
                .read_to_end(&mut content)
                .await
                .map_err(|_| S3Error::EmptyResponse)?;
            Ok(content)
        }

        /// Download an object from the given bucket.
        async fn download_object(&self, bucket: &str, key: &str) -> GetObjectOutput {
            // If an object does not exist, aws will return an error
            let req = GetObjectRequest {
                bucket: bucket.to_string(),
                key: key.to_string(),
                ..Default::default()
            };
            self.s3_client
                .get_object(req)
                .await
                .expect("download error")
        }
    }

    fn create_minio_setup() -> S3Settings {
        let region = Region::Custom {
            name: String::from("minio"),
            endpoint: String::from("http://localhost:9000"),
        };

        S3Settings {
            region,
            access_key: String::from("minio"),
            secret_access_key: String::from("minio123"),
            buckets: S3BucketsSettings::default(),
        }
    }

    pub async fn create_client() -> Client {
        let settings = create_minio_setup();
        let client = Client::new(settings).unwrap();
        client.create_global_models_bucket().await.unwrap();
        client.clear_bucket("global-models").await.unwrap();
        client
    }

    #[tokio::test]
    #[serial]
    async fn integration_test_upload_global_model() {
        let client = create_client().await;

        let global_model = create_global_model(10);
        let round_seed = hex::encode(RoundSeed::generate().as_slice());

        let res = client
            .upload_global_model(&format!("{}_{}", 1, round_seed), &global_model)
            .await;
        assert!(res.is_ok())
    }
}
