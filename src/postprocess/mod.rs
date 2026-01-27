// Processor categories - add new subdirectories here
pub mod common;
pub mod wandering_inn;

use std::collections::HashMap;
use std::sync::Arc;

// Re-export processors for convenience
pub use common::{StripColourProcessor, StripLinksProcessor};
pub use wandering_inn::MrshaWriteProcessor;

/// Trait for post-processors that transform chapter content
pub trait PostProcessor: Send + Sync {
    /// Get the processor name (used in config)
    fn name(&self) -> &str;

    /// Process the content and return the transformed result
    fn process(&self, content: &str) -> String;
}

/// Registry for post-processors
pub struct ProcessorRegistry {
    processors: HashMap<String, Arc<dyn PostProcessor>>,
}

impl ProcessorRegistry {
    /// Create a new registry with default processors
    pub fn new() -> Self {
        let mut registry = ProcessorRegistry {
            processors: HashMap::new(),
        };

        // Register common processors
        registry.register(Arc::new(StripColourProcessor));
        registry.register(Arc::new(StripLinksProcessor));

        // Register series-specific processors
        registry.register(Arc::new(MrshaWriteProcessor));

        registry
    }

    /// Register a processor
    pub fn register(&mut self, processor: Arc<dyn PostProcessor>) {
        self.processors
            .insert(processor.name().to_string(), processor);
    }

    /// Get a processor by name
    pub fn get(&self, name: &str) -> Option<&Arc<dyn PostProcessor>> {
        self.processors.get(name)
    }

    /// Apply a chain of processors to content
    #[allow(dead_code)]
    pub fn apply_chain(&self, content: &str, processor_names: &[String]) -> String {
        let mut result = content.to_string();

        for name in processor_names {
            if let Some(processor) = self.get(name) {
                result = processor.process(&result);
            }
        }

        result
    }

    /// Apply a single processor by name
    pub fn apply(&self, content: &str, processor_name: &str) -> String {
        if let Some(processor) = self.get(processor_name) {
            processor.process(content)
        } else {
            content.to_string()
        }
    }
}

impl Default for ProcessorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
