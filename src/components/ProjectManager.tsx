import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./ProjectManager.css";

interface Project {
  id: string;
  name: string;
  description: string;
  links: string[];
  path: string | null;
  created_at: string;
  updated_at: string;
}

interface ProjectStore {
  active_project_id: string | null;
  projects: Project[];
}

type FormMode = { type: "closed" } | { type: "add" } | { type: "edit"; project: Project };

export function ProjectManager() {
  const [store, setStore] = useState<ProjectStore>({ active_project_id: null, projects: [] });
  const [formMode, setFormMode] = useState<FormMode>({ type: "closed" });
  const [formName, setFormName] = useState("");
  const [formDesc, setFormDesc] = useState("");
  const [formLinks, setFormLinks] = useState<string[]>([]);
  const [formPath, setFormPath] = useState<string>("");
  const [formRepoUrl, setFormRepoUrl] = useState<string>("");
  const [analyzing, setAnalyzing] = useState(false);
  const [newLink, setNewLink] = useState("");
  const [uploadedFiles, setUploadedFiles] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  /// Unix-epoch-seconds string (e.g. "1700000000s") from the cached
  /// GitHub fetch for the project being edited. Null when no cache
  /// exists. Phase 2 Step 9.
  const [lastFetched, setLastFetched] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    refresh();
  }, []);

  async function refresh() {
    try {
      const data = await invoke<ProjectStore>("list_projects");
      setStore(data);
    } catch (e) {
      console.error("Failed to list projects:", e);
    }
  }

  function openAddForm() {
    setFormName("");
    setFormDesc("");
    setFormLinks([]);
    setFormPath("");
    setFormRepoUrl("");
    setNewLink("");
    setUploadedFiles([]);
    setLastFetched(null);
    setMsg(null);
    setFormMode({ type: "add" });
  }

  function openEditForm(project: Project) {
    setFormName(project.name);
    setFormDesc(project.description);
    setFormLinks(project.links || []);
    setFormPath(project.path || "");
    // Pre-fill the repo-url field if any saved link is a github URL —
    // saves the user from re-typing it when editing.
    const githubLink = (project.links || []).find((l) =>
      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()),
    );
    setFormRepoUrl(githubLink || "");
    setNewLink("");
    setUploadedFiles([]);
    setLastFetched(null);
    setMsg(null);
    setFormMode({ type: "edit", project });
    // Load the cached GitHub fetch timestamp (if any) for this project
    // so the "Last fetched:" line renders without a refresh round-trip.
    void invoke<string | null>("get_cached_context_timestamp", { id: project.id })
      .then((ts) => setLastFetched(ts ?? null))
      .catch((e) => {
        console.error("get_cached_context_timestamp failed:", e);
        setLastFetched(null);
      });
  }

  /// Manual refresh of the cached GitHub fetch. Only meaningful when
  /// the project is already saved (edit mode) and has at least one
  /// github.com link. Surfaces backend errors verbatim into the form's
  /// message area so the user can see why a refresh failed.
  async function handleRefreshContext() {
    if (formMode.type !== "edit") return;
    setRefreshing(true);
    setMsg(null);
    try {
      const ts = await invoke<string>("refresh_project_context", {
        id: formMode.project.id,
      });
      setLastFetched(ts);
      setMsg({ ok: true, text: "Project context refreshed." });
    } catch (e) {
      setMsg({ ok: false, text: `Refresh failed: ${e}` });
    } finally {
      setRefreshing(false);
    }
  }

  /// Format the backend's `"{seconds}s"` timestamp into a locale string.
  /// Returns `"never"` for `null` so callers don't need to branch.
  function formatLastFetched(raw: string | null): string {
    if (!raw) return "never";
    const match = /^(\d+)s$/.exec(raw);
    if (!match) return raw;
    const ms = Number(match[1]) * 1000;
    if (!Number.isFinite(ms) || ms <= 0) return raw;
    return new Date(ms).toLocaleString();
  }

  async function handleAnalyzeRepo() {
    const url = formRepoUrl.trim();
    if (!url) return;
    setAnalyzing(true);
    setMsg(null);
    try {
      const analyzed = await invoke<{
        name: string;
        description: string;
        default_branch: string;
        html_url: string;
      }>("analyze_github_repo", { url });
      // Always overwrite name; analyzed value wins. The user can still
      // edit it before saving.
      setFormName(analyzed.name);
      // Description: replace only if empty, otherwise append. Avoids
      // silently destroying notes the user already wrote.
      setFormDesc((prev) => (prev.trim() ? `${prev}\n\n${analyzed.description}` : analyzed.description));
      // Add the canonical html_url to links if not already present.
      setFormLinks((prev) =>
        prev.some((l) => l.trim() === analyzed.html_url) ? prev : [...prev, analyzed.html_url],
      );
      setMsg({ ok: true, text: `Analyzed ${analyzed.name}. Review and edit before saving.` });
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setAnalyzing(false);
    }
  }

  async function handleBrowseDirectory() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        setFormPath(selected);
      }
    } catch (e) {
      console.error("Directory picker failed:", e);
    }
  }

  function closeForm() {
    setFormMode({ type: "closed" });
    setMsg(null);
  }

  // ---- File Upload ----
  async function handleFileUpload() {
    try {
      const selected = await open({
        multiple: true,
        filters: [
          {
            name: "Documents",
            extensions: ["txt", "md", "json", "csv", "html", "xml", "yaml", "yml", "toml", "ini", "log", "cfg", "rs", "py", "js", "ts", "tsx", "jsx", "css", "scss", "java", "cpp", "c", "h", "go", "rb", "php", "sql", "sh", "bat", "ps1"],
          },
        ],
      });

      if (!selected) return;

      const paths = Array.isArray(selected) ? selected : [selected];
      
      for (const filePath of paths) {
        try {
          const content = await invoke<string>("read_file_content", { path: filePath });
          const fileName = filePath.split(/[/\\]/).pop() || filePath;
          
          // Append file content to description with a header
          const fileBlock = `\n\n--- File: ${fileName} ---\n${content}`;
          setFormDesc((prev) => prev + fileBlock);
          setUploadedFiles((prev) => [...prev, fileName]);
        } catch (e) {
          setMsg({ ok: false, text: `Failed to read ${filePath}: ${e}` });
        }
      }
    } catch (e) {
      console.error("File dialog error:", e);
    }
  }

  // ---- Links ----
  function addLink() {
    const trimmed = newLink.trim();
    if (!trimmed) return;
    setFormLinks((prev) => [...prev, trimmed]);
    setNewLink("");
  }

  function removeLink(index: number) {
    setFormLinks((prev) => prev.filter((_, i) => i !== index));
  }

  function handleLinkKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addLink();
    }
  }

  // ---- Save ----
  async function handleSave() {
    if (!formName.trim()) return;
    setBusy(true);
    setMsg(null);
    try {
      const path = formPath.trim() || null;
      if (formMode.type === "add") {
        await invoke("add_project", { name: formName, description: formDesc, path });
        // If links were added, immediately update the project with links
        const data = await invoke<ProjectStore>("list_projects");
        const newProject = data.projects[data.projects.length - 1];
        if (newProject && formLinks.length > 0) {
          await invoke("update_project", {
            id: newProject.id,
            name: formName,
            description: formDesc,
            links: formLinks,
            path,
          });
        }
        setMsg({ ok: true, text: "Project added!" });
      } else if (formMode.type === "edit") {
        await invoke("update_project", {
          id: formMode.project.id,
          name: formName,
          description: formDesc,
          links: formLinks,
          path,
        });
        setMsg({ ok: true, text: "Project updated!" });
      }
      await refresh();
      setTimeout(() => closeForm(), 600);
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete(id: string) {
    if (!confirm("Delete this project?")) return;
    try {
      await invoke("delete_project", { id });
      await refresh();
    } catch (e) {
      console.error("Delete failed:", e);
    }
  }

  async function handleSetActive(id: string) {
    try {
      await invoke("set_active_project", { id });
      await refresh();
    } catch (e) {
      console.error("Set active failed:", e);
    }
  }

  const activeProject = store.projects.find((p) => p.id === store.active_project_id);

  return (
    <div className="pm-container">
      {/* Header */}
      <div className="pm-header">
        <div>
          <h1 className="pm-title">
            Project Context
            <span className="pm-title-beta" aria-label="Beta feature">Beta</span>
          </h1>
          <p className="pm-subtitle">
            Add project descriptions to make prompt enhancements context-aware
          </p>
        </div>
        <button className="pm-add-btn" onClick={openAddForm}>
          + Add Project
        </button>
      </div>

      {/* Active project banner */}
      {activeProject && (
        <div className="pm-active-banner">
          <div className="pm-active-dot" />
          <span className="pm-active-label">
            Active: <span className="pm-active-name">{activeProject.name}</span>
          </span>
        </div>
      )}

      {/* Project list or empty state */}
      {store.projects.length === 0 ? (
        <div className="pm-empty">
          <div className="pm-empty-icon">📁</div>
          <h3>No projects yet</h3>
          <p>
            Add a project description so PerfectPrompt knows what you're building.
            The AI will generate smarter questions and more relevant prompts.
          </p>
        </div>
      ) : (
        <div className="pm-list">
          {store.projects.map((p) => (
            <div
              key={p.id}
              className={`pm-card ${p.id === store.active_project_id ? "active" : ""}`}
            >
              <div className="pm-card-header">
                <div className="pm-card-name">
                  {p.name}
                  {p.id === store.active_project_id && (
                    <span className="pm-card-badge">Active</span>
                  )}
                </div>
                <div className="pm-card-actions">
                  {p.id !== store.active_project_id && (
                    <button
                      className="pm-activate-btn"
                      onClick={() => handleSetActive(p.id)}
                    >
                      Set Active
                    </button>
                  )}
                  <button onClick={() => openEditForm(p)}>Edit</button>
                  <button
                    className="pm-delete-btn"
                    onClick={() => handleDelete(p.id)}
                  >
                    Delete
                  </button>
                </div>
              </div>
              <div className="pm-card-desc">
                {p.description || "No description provided."}
              </div>
              {p.links && p.links.length > 0 && (
                <div className="pm-card-links">
                  🔗 {p.links.length} link{p.links.length > 1 ? "s" : ""} attached
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Add/Edit Form Modal */}
      {formMode.type !== "closed" && (
        <div className="pm-form-overlay" onClick={closeForm}>
          <div className="pm-form" onClick={(e) => e.stopPropagation()}>
            <h3>{formMode.type === "add" ? "Add New Project" : "Edit Project"}</h3>

            {msg && (
              <div className={`pm-msg ${msg.ok ? "pm-msg-ok" : "pm-msg-err"}`}>
                {msg.text}
              </div>
            )}

            <label>Repo URL <span className="pm-optional">(paste & analyze to auto-fill)</span></label>
            <div className="pm-path-row">
              <input
                type="text"
                placeholder="https://github.com/owner/repo"
                value={formRepoUrl}
                onChange={(e) => setFormRepoUrl(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void handleAnalyzeRepo();
                  }
                }}
                disabled={busy || analyzing}
                className="pm-path-input"
                autoFocus
              />
              <button
                className="pm-link-add-btn"
                onClick={() => void handleAnalyzeRepo()}
                disabled={busy || analyzing || !formRepoUrl.trim()}
                type="button"
              >
                {analyzing ? "Analyzing…" : "Analyze"}
              </button>
            </div>

            {formMode.type === "edit" && (
              <div className="pm-refresh-row">
                <span className="pm-refresh-label">
                  Project context cache —{" "}
                  <span className="pm-refresh-timestamp">
                    Last fetched: {formatLastFetched(lastFetched)}
                  </span>
                </span>
                <button
                  className="pm-link-add-btn"
                  onClick={() => void handleRefreshContext()}
                  disabled={
                    busy ||
                    refreshing ||
                    !formLinks.some((l) =>
                      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()) ||
                      /^git@github\.com:/i.test(l.trim()),
                    )
                  }
                  type="button"
                  title={
                    formLinks.some((l) =>
                      /^(https?:\/\/(www\.)?)?github\.com\//i.test(l.trim()) ||
                      /^git@github\.com:/i.test(l.trim()),
                    )
                      ? "Re-fetch and overwrite the cached GitHub context"
                      : "Add a github.com link to enable refresh"
                  }
                >
                  {refreshing ? "Refreshing…" : "Refresh project context"}
                </button>
              </div>
            )}

            <label>Project Name</label>
            <input
              type="text"
              placeholder="e.g. PerfectPrompt, MyShopApp, PortfolioSite..."
              value={formName}
              onChange={(e) => setFormName(e.target.value)}
              disabled={busy}
            />

            <label>Project Directory <span className="pm-optional">(optional)</span></label>
            <div className="pm-path-row">
              <input
                type="text"
                placeholder="C:\path\to\project — enables file-aware enhancement"
                value={formPath}
                onChange={(e) => setFormPath(e.target.value)}
                disabled={busy}
                className="pm-path-input"
              />
              <button
                className="pm-link-add-btn"
                onClick={() => void handleBrowseDirectory()}
                disabled={busy}
                type="button"
              >
                Browse…
              </button>
            </div>

            <label>Project Description</label>
            <textarea
              placeholder={`Describe your project in detail. Include:\n\n• Tech stack (e.g. React, Tauri, Node.js)\n• What the project does\n• Key features and architecture\n• File structure overview\n• Any conventions or patterns used\n\nThe more detail, the better the AI understands your project.`}
              value={formDesc}
              onChange={(e) => setFormDesc(e.target.value)}
              disabled={busy}
            />

            {/* File Upload */}
            <div className="pm-upload-section">
              <button className="pm-upload-btn" onClick={handleFileUpload} disabled={busy} type="button">
                📄 Upload Files
              </button>
              <span className="pm-upload-hint">
                .txt, .md, .json, .csv, .html, code files, and more
              </span>
            </div>
            {uploadedFiles.length > 0 && (
              <div className="pm-uploaded-list">
                {uploadedFiles.map((f, i) => (
                  <span key={i} className="pm-uploaded-file">✓ {f}</span>
                ))}
              </div>
            )}

            {/* Links */}
            <label style={{ marginTop: 16 }}>Project Links</label>
            <div className="pm-links-input-row">
              <input
                type="text"
                placeholder="https://github.com/your-project or any relevant URL..."
                value={newLink}
                onChange={(e) => setNewLink(e.target.value)}
                onKeyDown={handleLinkKeyDown}
                disabled={busy}
                className="pm-link-input"
              />
              <button
                className="pm-link-add-btn"
                onClick={addLink}
                disabled={busy || !newLink.trim()}
                type="button"
              >
                Add
              </button>
            </div>
            {formLinks.length > 0 && (
              <div className="pm-links-list">
                {formLinks.map((link, i) => (
                  <div key={i} className="pm-link-item">
                    <span className="pm-link-text">🔗 {link}</span>
                    <button
                      className="pm-link-remove"
                      onClick={() => removeLink(i)}
                      type="button"
                    >
                      ×
                    </button>
                  </div>
                ))}
              </div>
            )}

            <p className="pm-form-hint">
              💡 Tip: Upload README, docs, or code files. Add links to repos, designs, or docs.
            </p>

            <div className="pm-form-footer">
              <button className="pm-btn-cancel" onClick={closeForm} disabled={busy}>
                Cancel
              </button>
              <button
                className="pm-btn-save"
                onClick={handleSave}
                disabled={busy || !formName.trim()}
              >
                {busy ? "Saving..." : formMode.type === "add" ? "Add Project" : "Save Changes"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
