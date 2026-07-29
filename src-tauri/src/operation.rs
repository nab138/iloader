use serde::Serialize;
use tauri::{Emitter, Window};

use crate::error::AppError;

pub struct Operation<'a> {
    id: String,
    window: &'a Window,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationUpdate<'a> {
    update_type: &'a str,
    step_id: &'a str,
    extra_details: Option<AppError>,
    progress: Option<f64>,
    uploaded_bytes: Option<u64>,
    total_bytes: Option<u64>,
}

impl<'a> Operation<'a> {
    pub fn new(id: String, window: &'a Window) -> Operation<'a> {
        Operation { id, window }
    }

    pub fn move_on(&self, old_id: &str, new_id: &str) -> Result<(), AppError> {
        self.complete(old_id)?;
        self.start(new_id)
    }

    pub fn start(&self, id: &str) -> Result<(), AppError> {
        self.window
            .emit(
                &format!("operation_{}", self.id),
                OperationUpdate {
                    update_type: "started",
                    step_id: id,
                    extra_details: None,
                    progress: None,
                    uploaded_bytes: None,
                    total_bytes: None,
                },
            )
            .map_err(|e| AppError::OperationUpdate(e.to_string()))
    }

    pub fn complete(&self, id: &str) -> Result<(), AppError> {
        self.window
            .emit(
                &format!("operation_{}", self.id),
                OperationUpdate {
                    update_type: "finished",
                    step_id: id,
                    extra_details: None,
                    progress: Some(1.0),
                    uploaded_bytes: None,
                    total_bytes: None,
                },
            )
            .map_err(|e| AppError::OperationUpdate(e.to_string()))
    }

    pub fn fail<T>(&self, id: &str, error: AppError) -> Result<T, AppError> {
        self.window
            .emit(
                &format!("operation_{}", self.id),
                OperationUpdate {
                    update_type: "failed",
                    step_id: id,
                    extra_details: Some(error.clone()),
                    progress: None,
                    uploaded_bytes: None,
                    total_bytes: None,
                },
            )
            .map_err(|e| AppError::OperationUpdate(e.to_string()))?;
        Err(error)
    }

    pub fn progress(&self, id: &str, progress: f64) -> Result<(), AppError> {
        self.window
            .emit(
                &format!("operation_{}", self.id),
                OperationUpdate {
                    update_type: "progress",
                    step_id: id,
                    extra_details: None,
                    progress: Some(progress.clamp(0.0, 1.0)),
                    uploaded_bytes: None,
                    total_bytes: None,
                },
            )
            .map_err(|e| AppError::OperationUpdate(e.to_string()))
    }

    pub fn progress_bytes(&self, id: &str, uploaded_bytes: u64, total_bytes: u64) -> Result<(), AppError> {
        let normalized = if total_bytes == 0 {
            0.0
        } else {
            (uploaded_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0)
        };

        self.window
            .emit(
                &format!("operation_{}", self.id),
                OperationUpdate {
                    update_type: "progress",
                    step_id: id,
                    extra_details: None,
                    progress: Some(normalized),
                    uploaded_bytes: Some(uploaded_bytes),
                    total_bytes: Some(total_bytes),
                },
            )
            .map_err(|e| AppError::OperationUpdate(e.to_string()))
    }

    pub fn fail_if_err<T>(&self, id: &str, res: Result<T, AppError>) -> Result<T, AppError> {
        match res {
            Ok(t) => Ok(t),
            Err(e) => self.fail::<T>(id, e),
        }
    }
}
