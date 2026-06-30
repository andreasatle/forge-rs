use super::*;

/// A runner that writes a file using a non-bare repo as the artifact repo.
/// The workspace creation succeeds (git clone works from non-bare repos) but
/// `integrate()` fails because `check_bare_repository` rejects the repo.
struct NonBareRepoFixture {
    _temp: TempDirectory,
    artifact: Artifact,
}

impl NonBareRepoFixture {
    fn new(label: &str) -> Self {
        let temp = TempDirectory::new(label);
        let repo_path = temp.join("not-bare.git");
        fs::create_dir(&repo_path).expect("failed to create non-bare repo directory");
        git(&repo_path, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo_path, &["config", "user.name", "Test"]);
        git(
            &repo_path,
            &["config", "user.email", "test@example.invalid"],
        );
        fs::write(repo_path.join("artifact.txt"), "v1\n").expect("failed to write initial file");
        git(&repo_path, &["add", "artifact.txt"]);
        git(&repo_path, &["commit", "--quiet", "-m", "Initial"]);
        let commit_sha = git_output(&repo_path, &["rev-parse", "HEAD"]);
        let artifact = Artifact {
            repo_path,
            branch: "main".to_owned(),
            commit_sha,
        };
        Self {
            _temp: temp,
            artifact,
        }
    }
}

#[test]
fn scheduler_handler_maps_integration_error_to_failed_outcome() {
    let fix = NonBareRepoFixture::new("integrate-error-mapping");
    let original_sha = fix.artifact.commit_sha.clone();

    let runner = FileWritingRunner {
        path: "output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, fix.artifact);

    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    assert!(
        matches!(event, SchedulerEvent::IntegrationFailed { .. }),
        "integrate() error must map to IntegrationFailed; got: {event:#?}"
    );

    // Artifact commit must remain unchanged on integration failure.
    let current_sha = h
        .artifact()
        .expect("artifact must still be present")
        .commit_sha;
    assert_eq!(
        current_sha, original_sha,
        "artifact commit must not change when integration fails"
    );
}

#[test]
fn scheduler_handler_maps_integration_conflict_to_failed_outcome() {
    let (_temp, artifact) = fixture("handler-cas-conflict");
    let original_sha = artifact.commit_sha.clone();
    let repo_path = artifact.repo_path.clone();

    let runner = FileWritingRunner {
        path: "cas-output.txt".to_string(),
        content: "hello\n".to_string(),
    };
    let h = SchedulerHandler::with_artifact(runner, artifact);

    // Run the node to stash a pending update.
    h.handle_effect(SchedulerEffect::RunNode {
        node_id: NodeId("W".to_string()),
        kind: NodeKind::Work,
        objective: "write a file".to_string(),
        target_files: vec![],
        test_plan_context: TestPlanContext::default(),
        model_tier: ModelTier::Cheap,
        attempt: 0,
        retry_feedback: None,
    });

    // Advance the branch externally between RunNode and IntegrateWork.
    let advanced_sha = advance_branch_in_bare(&repo_path, "main");

    // Attempt to integrate the stale workspace.
    let event = h.handle_effect(SchedulerEffect::IntegrateWork {
        node_id: NodeId("W".to_string()),
        work: WorkOutput {
            summary: "wrote cas-output.txt".to_string(),
        },
        attempt: 0,
        target_files: vec![],
        validation_plan: None,
    });

    let SchedulerEvent::IntegrationFailed { failure, .. } = &event else {
        panic!("expected IntegrationFailed, got: {event:#?}");
    };

    assert!(
        failure.message.contains(&original_sha) || failure.message.contains(&advanced_sha),
        "failure reason must mention expected or actual commit SHA; got: {}",
        failure.message
    );

    // Branch must remain at the externally advanced commit.
    let tip = git_output(&repo_path, &["rev-parse", "HEAD"]);
    assert_eq!(
        tip, advanced_sha,
        "branch must remain at the externally advanced commit"
    );
}
