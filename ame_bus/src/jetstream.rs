#[async_trait::async_trait]
pub trait NatsJetStreamConsumer: Send + Sync {
    async fn get_or_create_consumer(
        stream: async_nats::jetstream::stream::Stream,
    ) -> anyhow::Result<
        async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
    > {
        let consumer = stream
            .get_or_create_consumer(
                Self::CONSUMER_NAME,
                async_nats::jetstream::consumer::pull::Config {
                    durable_name: Some(Self::CONSUMER_NAME.to_owned()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }
    async fn get_or_create_stream(
        &self,
        js: &async_nats::jetstream::Context,
    ) -> anyhow::Result<async_nats::jetstream::stream::Stream> {
        let stream = js
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: Self::STREAM_NAME.to_owned(),
                max_messages: 100_000,
                ..Default::default()
            })
            .await?;
        Ok(stream)
    }
    const STREAM_NAME: &'static str;
    const CONSUMER_NAME: &'static str;
}
