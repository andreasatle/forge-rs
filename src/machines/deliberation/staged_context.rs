use crate::artifacts::{ArtifactError, ArtifactRead, ArtifactUpdate, StagedArtifactView};
use crate::machines::deliberation::state::DeliberationRole;
use crate::machines::scheduler::NodeKind;
use crate::roles::TargetView;
use crate::roles::runner::RoleToolContext;

use super::handler::{DeliberationHandler, TARGET_VIEW_BUDGET};

impl<R> DeliberationHandler<R> {
    pub(crate) fn role_tool_context_and_target_views(
        &self,
        role: &DeliberationRole,
        target_files: &[String],
    ) -> Result<(Option<RoleToolContext>, Vec<TargetView>), ArtifactError> {
        if self.node_kind == NodeKind::Plan {
            return Ok((None, vec![]));
        }

        let Some(base) = &self.artifact_view else {
            return Ok((None, vec![]));
        };

        let view: Box<dyn ArtifactRead> = match role {
            DeliberationRole::Producer if self.work_attempt.is_some() => {
                Box::new(self.work_attempt.as_ref().unwrap().workspace.clone())
            }
            DeliberationRole::Producer => Box::new(base.clone()),
            DeliberationRole::Critic | DeliberationRole::Referee if self.work_attempt.is_some() => {
                Box::new(self.work_attempt.as_ref().unwrap().workspace.clone())
            }
            DeliberationRole::Critic | DeliberationRole::Referee => {
                let changes = self.accumulated_update.borrow().clone();
                let update = ArtifactUpdate { changes };
                Box::new(StagedArtifactView::from_update(base.clone(), &update)?)
            }
        };

        let target_views =
            crate::project::build_file_text_target_views(&*view, target_files, TARGET_VIEW_BUDGET);

        Ok((
            Some(RoleToolContext {
                artifact_view: view,
                writable_workspace: match role {
                    DeliberationRole::Producer => self
                        .work_attempt
                        .as_ref()
                        .map(|attempt| attempt.workspace.clone()),
                    DeliberationRole::Critic | DeliberationRole::Referee => None,
                },
            }),
            target_views,
        ))
    }
}
