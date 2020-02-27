use influxdb::{Client, Query, Timestamp, Type, WriteQuery};

pub enum MetricOwner {
    Coordinator,
    Participant,
}

pub struct InfluxDBMetricStore {
    client: Client,
}

impl InfluxDBMetricStore {
    pub fn new(host: &str, db_name: &str) -> InfluxDBMetricStore {
        InfluxDBMetricStore {
            client: Client::new(host, db_name),
        }
    }

    fn metrics_owner_to_string(&self, metrics_owner: MetricOwner) -> &'static str {
        match metrics_owner {
            MetricOwner::Coordinator => "coordinator",
            MetricOwner::Participant => "participant",
        }
    }

    async fn write(&self, metrics_owner: MetricOwner, fields: Vec<(String, Type)>) -> () {
        let mut write_query: WriteQuery =
            Query::write_query(Timestamp::Now, self.metrics_owner_to_string(metrics_owner));

        for (name, value) in fields {
            write_query = write_query.add_field(name, value);
        }

        // Submit the query to InfluxDB.
        match self.client.query(&write_query).await {
            Err(err) => eprintln!("{:?}", err),
            _  => ()
        };
    }

    async fn write_with_tags(
        &self,
        metrics_owner: MetricOwner,
        fields: Vec<(String, Type)>,
        tags: Vec<(String, String)>,
    ) -> () {
        let mut write_query =
            Query::write_query(Timestamp::Now, self.metrics_owner_to_string(metrics_owner));

        for (name, value) in fields {
            write_query = write_query.add_field(name, value);
        }

        for (name, value) in tags {
            write_query = write_query.add_tag(name, value);
        }

        //Submit the query to InfluxDB.
        match self.client.query(&write_query).await {
            Err(err) => eprintln!("{:?}", err),
            _  => ()
        };
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use tokio; 

    #[tokio::test]
    async fn test_write_metrics() {
        let metric_store = InfluxDBMetricStore::new("http://localhost:8086", "metrics");
        let fields = vec![(String::from("CPU"), Type::SignedInteger(1))];

        metric_store.write(MetricOwner::Coordinator, fields).await;
    }

    #[tokio::test]
    async fn test_write_metrics_with_tags() {
        let metric_store = InfluxDBMetricStore::new("http://localhost:8086", "metrics");
        let fields = vec![(String::from("CPU"), Type::SignedInteger(123))];
        let tags = vec![(String::from("ID"), String::from("1234-1234-1234-1234"))];

        metric_store.write_with_tags(MetricOwner::Coordinator, fields, tags).await;
    }
}
