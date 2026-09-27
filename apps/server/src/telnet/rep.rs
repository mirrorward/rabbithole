//! Authenticated offline reply upload, independent of file-library permissions.
use super::*;

pub(super) async fn upload<S>(
    t: &mut BbsTerminal<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    export_id: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if export_id.is_empty() || export_id.len() > 128 || export_id.chars().any(char::is_whitespace) {
        return t
            .write_str("\nUsage: qwk-reply <export_id from the QWK packet you used>\n")
            .await;
    }
    if let Err(error) = crate::qwk::preflight_rep_archive(shared, &authed.account, export_id).await
    {
        return failure(t, error).await;
    }
    if !shared.rate_allow(Scope::Account(authed.account.id), rl::TRANSFER) {
        return t.write_str("\nRate limited; slow down.\n").await;
    }
    let Some(bytes) = crate::zmodem::receive_rep(&mut t.stream, shared, authed).await? else {
        return Ok(());
    };
    match crate::qwk::ingest_rep_archive_for(shared, &authed.account, export_id, &bytes).await {
        Ok(report) => {
            t.write_str(&format!(
                "\nREP import: {} accepted, {} duplicate, {} rejected.\n",
                report.accepted,
                report.duplicates,
                report.rejected.len()
            ))
            .await?;
            for (subject, reason) in report.rejected.iter().take(20) {
                t.write_str(&format!(
                    "  Rejected: {} - {}\n",
                    safe_text(subject, 80),
                    safe_text(reason, 160)
                ))
                .await?;
            }
            if report.rejected.len() > 20 {
                t.write_str("  Additional rejections omitted (showing the first 20).\n")
                    .await?;
            }
            Ok(())
        }
        Err(error) => failure(t, error).await,
    }
}

fn safe_text(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

async fn failure<S: AsyncRead + AsyncWrite + Unpin>(
    t: &mut BbsTerminal<S>,
    error: crate::qwk::QwkGateError,
) -> io::Result<()> {
    let message = match error {
        crate::qwk::QwkGateError::Disabled => {
            "QWK offline mail is not enabled on this system.".into()
        }
        crate::qwk::QwkGateError::Forbidden => {
            "You need an enabled account with permission to post board replies.".into()
        }
        crate::qwk::QwkGateError::Internal(reason) => safe_text(&reason, 200),
        error => {
            tracing::warn!(%error, "telnet REP import unavailable");
            "The REP importer is unavailable; try again later.".into()
        }
    };
    t.write_str(&format!("\nREP import refused: {message}\n"))
        .await
}
