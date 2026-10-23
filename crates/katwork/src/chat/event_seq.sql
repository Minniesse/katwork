SELECT coalesce((
    SELECT min(e.seq)-1 FROM events e
    JOIN runs r ON r.id=e.run_id AND r.workspace_id=e.workspace_id
    WHERE e.workspace_id=t.workspace_id AND e.thread_id=t.id
        AND e.kind='RUN_STARTED' AND r.state='running'
    ), t.last_event_seq) FROM threads t WHERE t.workspace_id=$1 AND t.id=$2
