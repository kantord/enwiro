//! Library surface of the git cookbook. Currently exposes git-native
//! merge detection ([`detect`]) so the github cookbook can reuse it
//! without duplicating the logic (#302, helper-home option B).

pub mod detect;
