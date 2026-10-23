SELECT id, space_id, team_id, title FROM threads WHERE workspace_id=$1 AND space_id=$2 ORDER BY id DESC LIMIT $3
