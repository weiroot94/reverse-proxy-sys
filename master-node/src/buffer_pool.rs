use bytes::BytesMut;
use log::{trace, debug};
use std::{
    collections::VecDeque,
    sync::Arc,
};
use tokio::sync::Mutex as AsyncMutex;

pub const MAX_BUF_SIZE: usize = 8192;
pub const POOL_SIZE: usize = 50;

// Sharded Buffer Pool for high concurrency
#[derive(Clone)]
pub struct ShardedBufferPool {
    shards: Vec<Arc<BufferPoolShard>>,
}

impl ShardedBufferPool {
    pub fn new(num_shards: usize, pool_size: usize) -> Self {
        let shards = (0..num_shards)
            .map(|_| Arc::new(BufferPoolShard::new(pool_size)))
            .collect();
        Self { shards }
    }

    pub async fn get_buffer(&self, id: usize) -> BytesMut {
        let shard = &self.shards[id % self.shards.len()];
        shard.get_buffer().await
    }

    pub async fn return_buffer(&self, id: usize, buffer: BytesMut) {
        let shard = &self.shards[id % self.shards.len()];
        shard.return_buffer(buffer).await;
    }
}

struct BufferPoolShard {
    buffers: AsyncMutex<VecDeque<BytesMut>>,
}

impl BufferPoolShard {
    fn new(pool_size: usize) -> Self {
        let mut buffers = VecDeque::with_capacity(pool_size);
        for _ in 0..pool_size {
            buffers.push_back(BytesMut::with_capacity(MAX_BUF_SIZE));
        }
        Self {
            buffers: AsyncMutex::new(buffers),
        }
    }

    async fn get_buffer(&self) -> BytesMut {
        let mut buffers = self.buffers.lock().await;
        if let Some(mut buffer) = buffers.pop_front() {
            if buffer.capacity() < MAX_BUF_SIZE {
                trace!("Discarding undersized buffer, creating new one");
                buffer = BytesMut::with_capacity(MAX_BUF_SIZE);
            } else {
                buffer.clear(); // Prepare buffer for reuse
            }
            buffer
        } else {
            trace!("No available buffer, creating new one");
            BytesMut::with_capacity(MAX_BUF_SIZE)
        }
    }

    async fn return_buffer(&self, buffer: BytesMut) {
        let mut buffers = self.buffers.lock().await;
        if buffers.len() < POOL_SIZE {
            buffers.push_back(buffer);
        } else {
            debug!("Buffer pool full, discarding buffer");
        }
    }
}
