//! User entity and the authenticated-user view.

use serde::Serialize;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub email: String,
    pub password_hash: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
}

/// The user behind a valid session.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: String,
    pub email: String,
}

impl From<CurrentUser> for UserResponse {
    fn from(user: CurrentUser) -> Self {
        UserResponse {
            id: user.id,
            email: user.email,
        }
    }
}
