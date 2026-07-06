(function () {
  "use strict";

  var app = document.getElementById("app");
  var i18n = {};
  var currentLocale = "en";
  var localeOptions = [];
  var draftSaveTimer = null;
  var setupSessionId = "setup-ui-" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2, 10);
  var providerSetupLogListeners = {};
  var providerSetupDiagnostics = {};
  var PROVIDER_SETUP_EVENT_NAMES = [
    "greentic-provider-setup-state",
    "greentic-provider-setup-request-start",
    "greentic-provider-setup-request-success",
    "greentic-provider-setup-request-error",
    "greentic-provider-setup-action-start",
    "greentic-provider-setup-action-complete",
    "greentic-provider-setup-action-timeout",
    "greentic-provider-setup-result",
    "greentic-provider-setup-error",
    "greentic-provider-setup-device-login",
    "greentic-provider-setup-complete"
  ];

  function t(key, args) {
    var text = i18n[key] || key.replace(/^ui\./, "");
    if (args) {
      for (var i = 0; i < args.length; i++) {
        text = text.replace("{}", args[i]);
      }
    }
    return text;
  }

  function renderLocalePicker() {
    if (localeOptions.length === 0) return "";
    var html = '<select id="locale-picker" class="locale-picker">';
    localeOptions.forEach(function (loc) {
      var sel = loc.code === currentLocale ? " selected" : "";
      html += '<option value="' + esc(loc.code) + '"' + sel + '>' + esc(loc.label) + '</option>';
    });
    html += '</select>';
    return html;
  }

  function setupLocalePicker() {
    var picker = document.getElementById("locale-picker");
    if (!picker) return;
    picker.addEventListener("change", function () {
      currentLocale = picker.value;
      reloadWithLocale();
    });
  }

  var RTL_LOCALES = ["ar", "he", "fa", "ur"];

  function isRtl(locale) {
    return RTL_LOCALES.some(function (r) { return locale === r || locale.startsWith(r + "-"); });
  }

  function applyDirection() {
    document.documentElement.dir = isRtl(currentLocale) ? "rtl" : "ltr";
  }

  function reloadWithLocale() {
    applyDirection();
    fetch("/api/providers?locale=" + encodeURIComponent(currentLocale))
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.i18n) i18n = data.i18n;
        state.providerForms = {};
        (data.provider_forms || []).forEach(function (pf) {
          pf.questions = filterHiddenQuestions(pf.questions || []);
          state.providerForms[pf.provider_id] = pf;
        });
        state.sharedQuestions = filterHiddenQuestions(data.shared_questions || []);
        render();
      });
  }

  // Questions intentionally hidden from the GUI. Keep public_base_url visible:
  // setup-time OAuth/webhook registration may need a real HTTPS origin.
  var HIDDEN_QUESTION_IDS = [];
  function filterHiddenQuestions(questions) {
    return questions.filter(function (q) { return HIDDEN_QUESTION_IDS.indexOf(q.id) === -1; });
  }

  // ── State ──

  function makeScope(tenant, env, team) {
    var answers = {};
    state.providers.forEach(function (p) {
      answers[p.provider_id] = {};
      var form = state.providerForms[p.provider_id];
      if (form) {
        form.questions.forEach(function (q) {
          if (q.saved_value) answers[p.provider_id][q.id] = q.saved_value;
        });
      }
    });
    return {
      tenant: tenant || "demo",
      env: env || "dev",
      team: team || "",
      tunnel: state.defaultTunnel || (state.cloudDeploy ? "off" : "cloudflared"),
      answers: answers,
      sharedAnswers: {},
      providersDone: {},
      providerSetupStatus: {},
      publicBaseUrlStatus: {},
      sharedAnswersDone: false,
      executed: false,
    };
  }

  var state = {
    phase: "loading",
    // global
    providers: [],
    sharedQuestions: [],
    providerForms: {},
    bundlePath: "",
    detectedTenant: null,
    cloudDeploy: false,
    defaultTunnel: "cloudflared",
    // multi-scope
    scopes: [],
    currentScopeIdx: -1,
    // per-scope working state
    currentProvider: 0,
    result: null,
  };

  /** Get the scope currently being edited. */
  function cs() { return state.scopes[state.currentScopeIdx]; }

  function providerEnabled(scope, providerId) {
    var answers = scope.answers[providerId] || {};
    var value = answers.enabled;
    if (value === undefined || value === null || value === "") return true;
    if (typeof value === "boolean") return value;
    return !/^(false|0|no|off|disabled)$/i.test(String(value).trim());
  }

  function setProviderEnabled(scope, providerId, enabled) {
    var answers = scope.answers[providerId] || {};
    answers.enabled = !!enabled;
    scope.answers[providerId] = answers;
    if (!enabled) scope.providersDone[providerId] = true;
    else delete scope.providersDone[providerId];
  }

  function buildPersistableAnswers(scope) {
    var answers = {};
    state.providers.forEach(function (p) {
      var providerAnswers = {};
      var existing = scope.answers[p.provider_id] || {};
      Object.keys(existing).forEach(function (k) { providerAnswers[k] = existing[k]; });
      Object.keys(scope.sharedAnswers || {}).forEach(function (k) {
        if (providerAnswers[k] === undefined || providerAnswers[k] === null || providerAnswers[k] === "") {
          providerAnswers[k] = scope.sharedAnswers[k];
        }
      });
      if (Object.keys(providerAnswers).length > 0) {
        answers[p.provider_id] = providerAnswers;
      }
    });
    return answers;
  }

  function persistDraftNow() {
    var scope = cs();
    if (!scope) return Promise.resolve();
    var payload = {
      answers: buildPersistableAnswers(scope),
      tenant: scope.tenant,
      env: scope.env,
    };
    if (scope.team) payload.team = scope.team;
    if (scope.tunnel) payload.tunnel = scope.tunnel;
    if (Object.keys(payload.answers).length === 0 && !payload.tunnel) return Promise.resolve();
    return fetch("/api/draft", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    })
    .then(function (r) { return r.json(); })
    .then(function (res) {
      if (!res.ok) console.warn("Draft persistence failed:", res.error || "unknown error");
    })
    .catch(function (err) {
      console.warn("Draft persistence failed:", err.message);
    });
  }

  function scheduleDraftSave() {
    if (draftSaveTimer) clearTimeout(draftSaveTimer);
    draftSaveTimer = setTimeout(function () {
      draftSaveTimer = null;
      persistDraftNow();
    }, 500);
  }

  function render() {
    switch (state.phase) {
      case "loading": renderLoading(); break;
      case "dashboard": renderDashboard(); break;
      case "scope-edit": renderScopeEdit(); break;
      case "tunnel": renderTunnel(); break;
      case "providers": renderProviders(); break;
      case "shared": renderForm(state.sharedQuestions, t("ui.shared.title"), t("ui.shared.description"), null, submitShared); break;
      case "provider-form": renderProviderForm(); break;
      case "provider-setup-action": renderProviderSetupAction(); break;
      case "review": renderReview(); break;
      case "executing": renderExecuting(); break;
      case "result": renderResult(); break;
      case "export": renderExport(); break;
    }
  }

  // ── Loading ──

  function renderLoading() {
    app.innerHTML =
      '<div class="fade-in center-msg">' +
        '<div class="spinner"></div>' +
        '<p class="executing-text">' + esc(t("ui.discovering")) + '</p>' +
        '<p class="executing-sub">' + esc(t("ui.discovering_sub")) + '</p>' +
      '</div>';

    applyDirection();
    fetch("/api/locales")
      .then(function (r) { return r.json(); })
      .then(function (locData) {
        localeOptions = locData.locales || [];
        currentLocale = locData.current || "en";
        return fetch("/api/providers?locale=" + encodeURIComponent(currentLocale));
      })
      .then(function (r) { return r.json(); })
      .then(function (data) {
        if (data.i18n) i18n = data.i18n;
        state.bundlePath = data.bundle_path || "";
        state.providers = data.providers || [];
        state.providerForms = {};
        (data.provider_forms || []).forEach(function (pf) {
          pf.questions = filterHiddenQuestions(pf.questions || []);
          state.providerForms[pf.provider_id] = pf;
        });
        state.sharedQuestions = filterHiddenQuestions(data.shared_questions || []);

        if (state.providers.length === 0) {
          app.innerHTML =
            '<div class="fade-in center-msg">' +
              '<p class="executing-text">' + esc(t("ui.no_providers")) + '</p>' +
              '<p class="executing-sub">' + esc(t("ui.nothing_to_configure")) + '</p>' +
              '<br><button class="btn btn-ghost" id="btn-close-empty">' + esc(t("ui.close")) + '</button>' +
            '</div>';
          document.getElementById("btn-close-empty").addEventListener("click", shutdown);
          return;
        }

        // Fetch existing scopes (previously configured) and scope defaults in parallel
        return Promise.all([
          fetch("/api/existing-scopes").then(function (r) { return r.json(); }),
          fetch("/api/scope").then(function (r) { return r.json(); }),
        ]);
      })
      .then(function (results) {
        var existingData = results[0];
        var scopeData = results[1];
        state.detectedTenant = scopeData.detected_tenant || null;
        state.cloudDeploy = !!scopeData.cloud_deploy;
        state.defaultTunnel = scopeData.tunnel || (state.cloudDeploy ? "off" : "cloudflared");

        var existingScopes = existingData.scopes || [];
        if (existingScopes.length > 0) {
          existingScopes.sort(function (a, b) {
            return scopeMatches(a, scopeData) ? -1 : (scopeMatches(b, scopeData) ? 1 : 0);
          });
          // Restore previously configured scopes
          state.scopes = existingScopes.map(function (es) {
            var s = makeScope(es.tenant || "demo", es.env || "dev", es.team || "");
            if (es.answers) {
              Object.keys(es.answers).forEach(function (pid) {
                if (typeof es.answers[pid] === "object") {
                  s.answers[pid] = es.answers[pid];
                }
              });
            }
            if (es.providers_done) {
              es.providers_done.forEach(function (pid) { s.providersDone[pid] = true; });
            }
            if (es.provider_setup_status && typeof es.provider_setup_status === "object") {
              s.providerSetupStatus = es.provider_setup_status;
            }
            return s;
          });
          state.currentScopeIdx = Math.max(0, state.scopes.findIndex(function (s) { return scopeMatches(s, scopeData); }));
          state.phase = "dashboard";
        } else if (state.scopes.length === 0) {
          // No existing config — create fresh scope and go to edit
          state.scopes.push(makeScope(
            scopeData.tenant || "demo",
            scopeData.env || "dev",
            scopeData.team || ""
          ));
          state.currentScopeIdx = 0;
          state.currentProvider = 0;
          state.phase = "scope-edit";
        } else {
          state.phase = "dashboard";
        }
        render();
      })
      .catch(function (err) {
        app.innerHTML =
          '<div class="fade-in center-msg">' +
            '<p class="executing-text">Failed to discover providers</p>' +
            '<p class="executing-sub">' + esc(err.message) + '</p>' +
          '</div>';
      });
  }

  // ── Dashboard ──

  function renderDashboard() {
    var html =
      '<div class="fade-in">' +
        '<div class="brand">' +
          '<div class="brand-icon">' +
            '<svg width="32" height="32" viewBox="0 0 32 32" fill="none"><rect width="32" height="32" rx="8" fill="#25c39e"/><path d="M10 16.5L14 20.5L22 12.5" stroke="white" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/></svg>' +
          '</div>' +
          '<h1 class="brand-title">' + esc(t("ui.title")) + '</h1>' +
          '<p class="brand-desc">' + esc(t("ui.dashboard.description", [String(state.providers.length), state.bundlePath])) + '</p>' +
          renderLocalePicker() +
        '</div>';

    // Scope cards
    html += '<div class="provider-list">';
    state.scopes.forEach(function (scope, idx) {
      var configured = Object.keys(scope.providersDone).length;
      var total = state.providers.length;
      var statusClass = scope.executed ? "done" : (configured === total && total > 0 ? "done" : "pending");
      var statusText = scope.executed ? t("ui.dashboard.executed") : (configured + "/" + total + " " + t("ui.dashboard.configured"));
      html +=
        '<div class="provider-card scope-card" data-idx="' + idx + '">' +
          '<div class="prov-icon" style="background:#6366f1">' + esc(scope.tenant.charAt(0).toUpperCase()) + '</div>' +
          '<div style="flex:1">' +
            '<div class="prov-name">' + esc(scope.tenant) + '</div>' +
            '<div class="prov-domain">' + esc(scope.env) + (scope.team ? " / " + esc(scope.team) : "") + '</div>' +
          '</div>' +
          '<span class="prov-badge ' + statusClass + '">' + esc(statusText) + '</span>' +
          '<button class="btn btn-ghost btn-sm scope-delete" data-idx="' + idx + '" title="' + esc(t("ui.dashboard.delete")) + '">' +
            '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M8 6V4h8v2M19 6v14a2 2 0 01-2 2H7a2 2 0 01-2-2V6"/></svg>' +
          '</button>' +
        '</div>';
    });
    html += '</div>';

    // Import drop zone
    html +=
      '<div id="import-zone" class="import-zone">' +
        '<input type="file" id="import-file" accept=".json,.yaml,.yml" style="display:none" />' +
        '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" y1="3" x2="12" y2="15"/></svg>' +
        ' ' + esc(t("ui.import.dropzone")) +
      '</div>';

    // Action buttons
    html +=
      '<div style="display:flex;gap:.5rem;flex-wrap:wrap">' +
        '<button class="btn btn-secondary" id="btn-add-scope" style="flex:1">' + esc(t("ui.dashboard.add_scope")) + '</button>' +
        '<button class="btn btn-secondary" id="btn-export" style="flex:1">' + esc(t("ui.dashboard.export")) + '</button>' +
      '</div>' +
      '<div style="text-align:center;margin-top:.75rem"><button class="btn btn-ghost" id="btn-close-dash">' + esc(t("ui.close")) + '</button></div>' +
    '</div>';

    app.innerHTML = html;
    setupLocalePicker();

    // Scope card click → edit
    app.querySelectorAll(".scope-card").forEach(function (card) {
      card.addEventListener("click", function (e) {
        if (e.target.closest(".scope-delete")) return;
        var idx = parseInt(card.getAttribute("data-idx"), 10);
        state.currentScopeIdx = idx;
        state.currentProvider = 0;
        state.phase = "scope-edit";
        render();
      });
    });

    // Delete buttons
    app.querySelectorAll(".scope-delete").forEach(function (btn) {
      btn.addEventListener("click", function (e) {
        e.stopPropagation();
        var idx = parseInt(btn.getAttribute("data-idx"), 10);
        if (state.scopes.length <= 1) return; // keep at least one
        state.scopes.splice(idx, 1);
        render();
      });
    });

    document.getElementById("btn-add-scope").addEventListener("click", function () {
      var newScope = makeScope(
        state.detectedTenant || "demo",
        "dev",
        ""
      );
      state.scopes.push(newScope);
      state.currentScopeIdx = state.scopes.length - 1;
      state.currentProvider = 0;
      state.phase = "scope-edit";
      render();
    });

    document.getElementById("btn-export").addEventListener("click", function () {
      state.phase = "export";
      render();
    });

    // Import: file picker
    var importZone = document.getElementById("import-zone");
    var importFile = document.getElementById("import-file");
    importZone.addEventListener("click", function () { importFile.click(); });
    importFile.addEventListener("change", function () {
      if (importFile.files.length > 0) handleImportFile(importFile.files[0]);
    });
    // Import: drag & drop
    importZone.addEventListener("dragover", function (e) { e.preventDefault(); importZone.classList.add("drag-over"); });
    importZone.addEventListener("dragleave", function () { importZone.classList.remove("drag-over"); });
    importZone.addEventListener("drop", function (e) {
      e.preventDefault();
      importZone.classList.remove("drag-over");
      if (e.dataTransfer.files.length > 0) handleImportFile(e.dataTransfer.files[0]);
    });

    document.getElementById("btn-close-dash").addEventListener("click", shutdown);
  }

  // ── Import answers file ──

  function handleImportFile(file) {
    var reader = new FileReader();
    reader.onload = function () {
      try {
        var doc = JSON.parse(reader.result);
        processImportedDoc(doc);
      } catch (e) {
        alert(t("ui.import.parse_error") + ": " + e.message);
      }
    };
    reader.readAsText(file);
  }

  function processImportedDoc(doc) {
    // Check if encrypted
    if (docHasEncrypted(doc)) {
      var key = prompt(t("ui.import.password_prompt"));
      if (!key) return;
      fetch("/api/decrypt", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ doc: doc, key: key }),
      })
      .then(function (r) { return r.json(); })
      .then(function (res) {
        if (res.ok) {
          importScopesFromDoc(res.doc);
        } else {
          alert(t("ui.import.decrypt_failed") + ": " + (res.error || "unknown error"));
        }
      })
      .catch(function (err) { alert(t("ui.import.decrypt_failed") + ": " + err.message); });
    } else {
      importScopesFromDoc(doc);
    }
  }

  function docHasEncrypted(val) {
    if (!val || typeof val !== "object") return false;
    if (val.__greentic_encrypted__) return true;
    var keys = Array.isArray(val) ? val : Object.values(val);
    return keys.some(function (v) { return docHasEncrypted(v); });
  }

  function importScopesFromDoc(doc) {
    var imported = [];

    // Multi-scope format: { scopes: [...] }
    if (doc.scopes && Array.isArray(doc.scopes)) {
      doc.scopes.forEach(function (s) { imported.push(scopeFromAnswerDoc(s)); });
    }
    // Single scope format: { tenant, env, setup_answers: {...} }
    else if (doc.setup_answers && typeof doc.setup_answers === "object") {
      imported.push(scopeFromAnswerDoc(doc));
    }
    // Flat format: { "messaging-telegram": {...}, ... } (no metadata)
    else if (typeof doc === "object" && !doc.greentic_setup_version) {
      var s = makeScope("demo", "dev", "");
      Object.keys(doc).forEach(function (k) {
        if (typeof doc[k] === "object" && !Array.isArray(doc[k])) {
          s.answers[k] = doc[k];
        }
      });
      imported.push(s);
    }

    if (imported.length === 0) {
      alert(t("ui.import.no_scopes"));
      return;
    }

    // Replace existing scopes or append
    state.scopes = imported;
    state.phase = "dashboard";
    render();
  }

  function scopeFromAnswerDoc(doc) {
    var s = makeScope(
      doc.tenant || "demo",
      doc.env || "dev",
      doc.team || ""
    );
    if (doc.setup_answers && typeof doc.setup_answers === "object") {
      Object.keys(doc.setup_answers).forEach(function (pid) {
        if (typeof doc.setup_answers[pid] === "object") {
          s.answers[pid] = doc.setup_answers[pid];
          // Mark provider as done if it has non-empty answers
          var keys = Object.keys(doc.setup_answers[pid]);
          if (keys.length > 0) s.providersDone[pid] = true;
        }
      });
    }
    return s;
  }

  function scopeMatches(scope, expected) {
    if (!scope || !expected) return false;
    return (scope.tenant || "demo") === (expected.tenant || "demo") &&
      (scope.env || "dev") === (expected.env || "dev") &&
      normalizeTeam(scope.team) === normalizeTeam(expected.team);
  }

  function normalizeTeam(team) {
    var value = (team || "").trim();
    return value === "" || value === "default" ? "" : value;
  }

  // ── Scope edit ──

  function renderScopeEdit() {
    var scope = cs();
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(t("ui.scope.title")) + '</h2>' +
            '<p class="card-desc">' + esc(t("ui.scope.hint")) + '</p>' +
          '</div>' +
          '<div class="card-content"><div class="form-fields">';

    html +=
      '<div class="field">' +
        '<label class="field-label" for="f-scope-tenant">' + esc(t("ui.scope.tenant")) + '<span class="required">*</span></label>' +
        '<input type="text" id="f-scope-tenant" value="' + esc(scope.tenant) + '" />';
    if (state.detectedTenant) {
      html += '<p class="field-help">' + esc(t("ui.scope.detected_tenant", [state.detectedTenant])) + '</p>';
    }
    html += '</div>';

    html +=
      '<div class="field">' +
        '<label class="field-label" for="f-scope-env">' + esc(t("ui.scope.env")) + '<span class="required">*</span></label>' +
        '<select id="f-scope-env">' +
          '<option value="dev"' + (scope.env === "dev" ? " selected" : "") + '>dev</option>' +
          '<option value="local"' + (scope.env === "local" ? " selected" : "") + '>local</option>' +
          '<option value="test"' + (scope.env === "test" ? " selected" : "") + '>test</option>' +
          '<option value="staging"' + (scope.env === "staging" ? " selected" : "") + '>staging</option>' +
          '<option value="prod"' + (scope.env === "prod" ? " selected" : "") + '>prod</option>' +
        '</select>' +
        '<p class="field-help">' + esc(t("ui.scope.env_help")) + '</p>' +
      '</div>';

    html +=
      '<div class="field">' +
        '<label class="field-label" for="f-scope-team">' + esc(t("ui.scope.team")) + '</label>' +
        '<input type="text" id="f-scope-team" value="' + esc(scope.team) + '" placeholder="default" />' +
        '<p class="field-help">' + esc(t("ui.scope.team_help")) + '</p>' +
      '</div>';

    html +=
          '</div></div>' +
          '<div class="card-footer">' +
            '<button class="btn btn-primary btn-lg" id="btn-scope-continue" style="width:100%">' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;

    document.getElementById("btn-back").addEventListener("click", function () {
      state.phase = "dashboard";
      render();
    });

    document.getElementById("btn-scope-continue").addEventListener("click", function () {
      var tenantEl = document.getElementById("f-scope-tenant");
      var tenant = tenantEl.value.trim();
      if (!tenant) { tenantEl.classList.add("input-error"); tenantEl.focus(); return; }
      scope.tenant = tenant;
      scope.env = document.getElementById("f-scope-env").value;
      scope.team = document.getElementById("f-scope-team").value.trim();
      scope.tunnel = state.cloudDeploy ? "off" : scope.tunnel;
      state.phase = state.cloudDeploy ? "providers" : "tunnel";
      render();
    });
  }

  // ── Tunnel configuration ──

  function renderTunnel() {
    var scope = cs();
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">Tunnel</h2>' +
            '<p class="card-desc">External messaging channels (Webex, Telegram, Slack, etc.) need a public URL to deliver webhooks to your local machine. Choose a tunnel service.</p>' +
          '</div>' +
          '<div class="card-content"><div class="form-fields">' +
            '<div class="field">' +
              '<label class="field-label">Tunnel service</label>' +
              '<div class="tunnel-options">' +
                '<label class="tunnel-option' + (scope.tunnel === "cloudflared" ? ' selected' : '') + '">' +
                  '<input type="radio" name="tunnel" value="cloudflared"' + (scope.tunnel === "cloudflared" ? ' checked' : '') + ' />' +
                  '<div><strong>Cloudflare Tunnel</strong><br/><span style="opacity:.7;font-size:.85rem">Free, no account needed. Auto-installs if missing.</span></div>' +
                '</label>' +
                '<label class="tunnel-option' + (scope.tunnel === "ngrok" ? ' selected' : '') + '">' +
                  '<input type="radio" name="tunnel" value="ngrok"' + (scope.tunnel === "ngrok" ? ' checked' : '') + ' />' +
                  '<div><strong>ngrok</strong><br/><span style="opacity:.7;font-size:.85rem">Requires ngrok account and binary installed.</span></div>' +
                '</label>' +
                '<label class="tunnel-option' + (scope.tunnel === "off" ? ' selected' : '') + '">' +
                  '<input type="radio" name="tunnel" value="off"' + (scope.tunnel === "off" ? ' checked' : '') + ' />' +
                  '<div><strong>No tunnel</strong><br/><span style="opacity:.7;font-size:.85rem">Local only. External webhooks will not work.</span></div>' +
                '</label>' +
              '</div>' +
            '</div>' +
          '</div></div>' +
          '<div class="card-footer">' +
            '<button class="btn btn-primary btn-lg" id="btn-tunnel-continue" style="width:100%">' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;

    // Highlight selected option
    document.querySelectorAll('input[name="tunnel"]').forEach(function (radio) {
      radio.addEventListener("change", function () {
        document.querySelectorAll(".tunnel-option").forEach(function (el) { el.classList.remove("selected"); });
        radio.closest(".tunnel-option").classList.add("selected");
      });
    });

    document.getElementById("btn-back").addEventListener("click", function () {
      state.phase = "scope-edit";
      render();
    });

    document.getElementById("btn-tunnel-continue").addEventListener("click", function () {
      var selected = document.querySelector('input[name="tunnel"]:checked');
      scope.tunnel = selected ? selected.value : "cloudflared";
      persistDraftNow().finally(function () {
        state.phase = "providers";
        render();
      });
    });
  }

  // ── Provider list ──

  function renderProviders() {
    var scope = cs();
    var allDone = state.providers.every(function (p) {
      return !providerEnabled(scope, p.provider_id) || scope.providersDone[p.provider_id];
    });

    var html =
      '<div class="fade-in">' +
        '<div class="brand">' +
          '<div class="brand-icon">' +
            '<svg width="32" height="32" viewBox="0 0 32 32" fill="none"><rect width="32" height="32" rx="8" fill="#25c39e"/><path d="M10 16.5L14 20.5L22 12.5" stroke="white" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/></svg>' +
          '</div>' +
          '<h1 class="brand-title">' + esc(t("ui.title")) + '</h1>' +
          '<p class="brand-desc">' + esc(t("ui.description", [String(state.providers.length), state.bundlePath])) + '</p>' +
          '<p class="brand-desc" style="font-size:.8rem;opacity:.7">tenant=' + esc(scope.tenant) + ' env=' + esc(scope.env) + (scope.team ? ' team=' + esc(scope.team) : '') + '</p>' +
        '</div>' +
        '<div class="provider-list">';

    state.providers.forEach(function (p, idx) {
      var enabled = providerEnabled(scope, p.provider_id);
      var done = scope.providersDone[p.provider_id];
      var form = state.providerForms[p.provider_id];
      var qCount = form ? form.questions.length : 0;
      var setupKind = p.setup_web_component ? "web setup" : qCount + ' ' + esc(t("ui.questions"));
      var displayName = formatProviderName(p);
      html +=
        '<div class="provider-card clickable' + (!enabled ? ' disabled' : '') + '" data-prov-idx="' + idx + '">' +
          '<div class="prov-icon">' + esc(displayName.charAt(0)) + '</div>' +
          '<div>' +
            '<div class="prov-name">' + esc(displayName) + '</div>' +
            '<div class="prov-domain">' + esc(p.domain) + ' &middot; ' + (enabled ? setupKind : 'disabled') + '</div>' +
          '</div>' +
          '<label class="provider-enable" data-provider-enable-wrap title="Enable provider">' +
            '<input type="checkbox" data-provider-enable="' + esc(p.provider_id) + '"' + (enabled ? ' checked' : '') + ' />' +
            '<span></span>' +
          '</label>' +
          '<span class="prov-badge ' + (!enabled ? 'disabled' : (done ? 'done' : 'pending')) + '">' + (!enabled ? 'disabled' : (done ? esc(t("ui.done")) : esc(t("ui.pending")))) + '</span>' +
        '</div>';
    });

    html += '</div>';

    if (state.sharedQuestions.length > 0 && !scope.sharedAnswersDone) {
      html += '<button class="btn btn-primary btn-lg" id="btn-start" style="width:100%">' + esc(t("ui.start_config")) + '</button>';
    } else {
      // Always offer sequential edit starting from the first provider
      html += '<button class="btn btn-primary btn-lg" id="btn-next-prov" data-idx="0" style="width:100%">' + esc(t("ui.configure", [formatProviderName(state.providers[0])])) + '</button>';
    }

    html +=
      '<div style="text-align:center;margin-top:.75rem;display:flex;justify-content:center;gap:.5rem">' +
        '<button class="btn btn-ghost" id="btn-back-scope">' + esc(t("ui.back")) + '</button>' +
        (allDone ? '<button class="btn btn-ghost" id="btn-review">' + esc(t("ui.review_execute")) + '</button>' : '') +
      '</div></div>';

    app.innerHTML = html;
    setupLocalePicker();

    var startBtn = document.getElementById("btn-start");
    if (startBtn) startBtn.addEventListener("click", function () { state.phase = "shared"; render(); });

    var nextBtn = document.getElementById("btn-next-prov");
    if (nextBtn) nextBtn.addEventListener("click", function () {
      state.currentProvider = parseInt(nextBtn.getAttribute("data-idx"), 10);
      state.phase = "provider-form";
      render();
    });

    var reviewBtn = document.getElementById("btn-review");
    if (reviewBtn) reviewBtn.addEventListener("click", function () { state.phase = "review"; render(); });

    // Click on any provider card to edit it
    document.querySelectorAll(".provider-card.clickable").forEach(function (card) {
      card.addEventListener("click", function () {
        var idx = parseInt(card.getAttribute("data-prov-idx"), 10);
        state.currentProvider = idx;
        state.phase = "provider-form";
        render();
      });
    });

    document.querySelectorAll('[data-provider-enable-wrap]').forEach(function (wrap) {
      wrap.addEventListener("click", function (e) { e.stopPropagation(); });
    });
    document.querySelectorAll('[data-provider-enable]').forEach(function (input) {
      input.addEventListener("click", function (e) { e.stopPropagation(); });
      input.addEventListener("change", function (e) {
        e.stopPropagation();
        setProviderEnabled(scope, input.getAttribute("data-provider-enable"), input.checked);
        scheduleDraftSave();
        render();
      });
    });

    document.getElementById("btn-back-scope").addEventListener("click", function () {
      state.phase = state.cloudDeploy ? "scope-edit" : "tunnel";
      render();
    });
  }

  // ── Form rendering (reusable) ──

  function renderForm(questions, title, desc, backPhase, onSubmit, backFn) {
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(title) + '</h2>' +
            '<p class="card-desc">' + esc(desc) + '</p>' +
          '</div>' +
          '<div class="card-content"><div id="form-area" class="form-fields">';

    var currentGroup = null;
    questions.forEach(function (q) {
      if (q.group && q.group !== currentGroup) {
        if (currentGroup !== null) html += '</div>';
        currentGroup = q.group;
        html += '<div class="form-group"><h4 class="form-group-title">' + esc(q.group) + '</h4>';
      } else if (!q.group && currentGroup !== null) {
        html += '</div>';
        currentGroup = null;
      }
      html += renderQuestion(q);
    });
    if (currentGroup !== null) html += '</div>';

    html +=
          '</div></div>' +
          '<div class="card-footer card-footer-split">' +
            '<button class="btn btn-secondary" id="btn-prev">' +
              '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
              ' ' + esc(t("ui.back")) +
            '</button>' +
            '<button class="btn btn-primary" id="btn-submit">' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;
    // Widen the centered container when the form contains a table (kind:
    // List) — 7 columns of inputs need more horizontal room than the
    // default 620px. Removed when navigating to a non-table form.
    var hasTable = questions.some(function (q) {
      return q.kind === "List" && q.list_columns && q.list_columns.length > 0;
    });
    document.body.classList.toggle("has-wide-form", !!hasTable);
    restoreFormValues(questions);
    setupTableQuestions(questions);
    setupVisibility(questions);
    // Event delegation so dynamically-added inputs (e.g. locale rows
    // appended after clicking "+ Add" on the row-level language toolbar)
    // also trigger collect + autosave. Attaching listeners only to inputs
    // that exist at render time silently dropped post-render edits.
    var formArea = document.getElementById("form-area");
    if (formArea) {
      var delegateHandler = function (e) {
        var t = e.target;
        if (!t) return;
        var tag = (t.tagName || "").toLowerCase();
        if (tag !== "input" && tag !== "select" && tag !== "textarea") return;
        collectFormValues(questions);
        scheduleDraftSave();
      };
      formArea.addEventListener("input", delegateHandler);
      formArea.addEventListener("change", delegateHandler);
    }

    var goBack = backFn || function () { state.phase = backPhase || "providers"; render(); };
    document.getElementById("btn-back").addEventListener("click", function () {
      collectFormValues(questions);
      persistDraftNow().finally(goBack);
    });
    document.getElementById("btn-prev").addEventListener("click", function () {
      collectFormValues(questions);
      persistDraftNow().finally(goBack);
    });
    document.getElementById("btn-submit").addEventListener("click", function () {
      if (!validateForm(questions)) return;
      collectFormValues(questions);
      persistDraftNow().finally(onSubmit);
    });
  }

  function renderQuestion(q) {
    var visAttr = "";
    if (q.visible_if) {
      visAttr = ' data-vis-field="' + esc(q.visible_if.field) + '" data-vis-eq="' + esc(q.visible_if.eq || "") + '"';
    }
    var html = '<div class="field" id="field-' + esc(q.id) + '"' + visAttr + '>';
    if (q.kind === "List" && q.list_columns && q.list_columns.length > 0) {
      // Repeating-row table input. Each row is one entry (e.g. nav link).
      // The user clicks "+ Add row" to grow the list and the trash icon
      // to drop a row. min_rows / max_rows enforce bounds at submit time.
      html += '<label class="field-label">' + esc(q.title);
      if (q.required) html += '<span class="required">*</span>';
      html += '</label>';
      if (q.help) html += '<p class="field-help">' + esc(q.help) + '</p>';
      html += '<div class="table-wrap" id="t-' + esc(q.id) + '" data-q-id="' + esc(q.id) + '"';
      if (q.min_rows != null) html += ' data-min-rows="' + esc(String(q.min_rows)) + '"';
      if (q.max_rows != null) html += ' data-max-rows="' + esc(String(q.max_rows)) + '"';
      html += '>';
      html += '<table class="row-table"><thead><tr>';
      q.list_columns.forEach(function (c) {
        var thAttrs = ' data-col="' + esc(c.id) + '"';
        if (c.multilingual) thAttrs += ' data-multilingual="true"';
        if (c.kind === 'Boolean') thAttrs += ' data-boolean="true"';
        html += '<th' + thAttrs + '>' + esc(c.title) + (c.required ? '<span class="required">*</span>' : '') + '</th>';
      });
      html += '<th class="row-table__action"></th></tr></thead>';
      html += '<tbody data-rows></tbody></table>';
      html += '<button type="button" class="btn-secondary" data-add-row>+ Add row</button>';
      html += '</div>';
    } else if (q.kind === "Boolean") {
      html +=
        '<div class="field-row">' +
          '<label class="field-label" for="f-' + esc(q.id) + '">' + esc(q.title) + '</label>' +
          '<label class="switch"><input type="checkbox" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '" /><span class="switch-slider"></span></label>' +
        '</div>';
    } else {
      html += '<label class="field-label" for="f-' + esc(q.id) + '">' + esc(q.title);
      if (q.required) html += '<span class="required">*</span>';
      html += '</label>';
      var ph = q.placeholder || q.default_value || "";
      if (q.choices && q.choices.length > 0) {
        html += '<select id="f-' + esc(q.id) + '" name="' + esc(q.id) + '">';
        q.choices.forEach(function (c) { html += '<option value="' + esc(c) + '">' + esc(c) + '</option>'; });
        html += '</select>';
      } else if (q.secret) {
        html += '<input type="password" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '"' + (ph ? ' placeholder="' + esc(ph) + '"' : '') + ' />';
      } else {
        html += '<input type="text" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '"' + (ph ? ' placeholder="' + esc(ph) + '"' : '') + ' />';
      }
      if (q.help || q.docs_url || q.create_url) {
        html += '<div class="field-meta">';
        if (q.help) html += '<p class="field-help">' + esc(q.help) + '</p>';
        if (q.create_url) html += '<a class="field-create" href="' + esc(q.create_url) + '" target="_blank" rel="noopener">Don\'t have one? Create it ↗</a>';
        if (q.docs_url) html += '<a class="field-docs" href="' + esc(q.docs_url) + '" target="_blank" rel="noopener">Setup Guide ↗</a>';
        html += '</div>';
      }
    }
    html += '</div>';
    return html;
  }

  /// Locale codes ↔ native names. Mirrors `SUPPORTED_LOCALES` in the
  /// webchat-gui SPA's `runtime-bootstrap.js` so the wizard offers exactly
  /// the same set of translations that ship in the i18n bundle. Keep
  /// these two lists in sync when adding/removing a locale.
  var I18N_LOCALES = [
    ['ar', 'العربية'], ['ar-AE', 'العربية (الإمارات)'], ['ar-DZ', 'العربية (الجزائر)'],
    ['ar-EG', 'العربية (مصر)'], ['ar-IQ', 'العربية (العراق)'], ['ar-MA', 'العربية (المغرب)'],
    ['ar-SA', 'العربية (السعودية)'], ['ar-SD', 'العربية (السودان)'], ['ar-SY', 'العربية (سوريا)'],
    ['ar-TN', 'العربية (تونس)'],
    ['ay', "Aymar aru"], ['bg', 'Български'], ['bn', 'বাংলা'], ['cs', 'Čeština'],
    ['da', 'Dansk'], ['de', 'Deutsch'], ['el', 'Ελληνικά'], ['en', 'English'],
    ['en-GB', 'English (UK)'], ['es', 'Español'], ['et', 'Eesti'], ['fa', 'فارسی'],
    ['fi', 'Suomi'], ['fr', 'Français'], ['gn', "Avañe'ẽ"], ['gu', 'ગુજરાતી'],
    ['hi', 'हिन्दी'], ['hr', 'Hrvatski'], ['ht', 'Kreyòl ayisyen'], ['hu', 'Magyar'],
    ['id', 'Bahasa Indonesia'], ['it', 'Italiano'], ['ja', '日本語'], ['km', 'ខ្មែរ'],
    ['kn', 'ಕನ್ನಡ'], ['ko', '한국어'], ['lo', 'ລາວ'], ['lt', 'Lietuvių'],
    ['lv', 'Latviešu'], ['ml', 'മലയാളം'], ['mr', 'मराठी'], ['ms', 'Bahasa Melayu'],
    ['my', 'မြန်မာ'], ['nah', 'Nāhuatl'], ['ne', 'नेपाली'], ['nl', 'Nederlands'],
    ['no', 'Norsk'], ['pa', 'ਪੰਜਾਬੀ'], ['pl', 'Polski'], ['pt', 'Português'],
    ['qu', 'Runa simi'], ['ro', 'Română'], ['ru', 'Русский'], ['si', 'සිංහල'],
    ['sk', 'Slovenčina'], ['sr', 'Српски'], ['sv', 'Svenska'], ['ta', 'தமிழ்'],
    ['te', 'తెలుగు'], ['th', 'ไทย'], ['tl', 'Tagalog'], ['tr', 'Türkçe'],
    ['uk', 'Українська'], ['ur', 'اردو'], ['vi', 'Tiếng Việt'], ['zh', '中文']
  ];

  /// Build the inner DOM for a multilingual cell. `existing` is either a
  /// plain string (single-locale) or an object `{en: "...", id: "...", ...}`.
  /// The cell only renders locale sub-rows; the "+ language" picker is
  /// hoisted to a row-level toolbar so one click adds the locale to every
  /// multilingual column in the row at once.
  function buildI18nCell(td, c, existing) {
    var wrap = document.createElement('div');
    wrap.className = 'i18n-cell';
    wrap.dataset.col = c.id;
    wrap.dataset.i18n = '1';

    var entries = {};
    if (existing && typeof existing === 'object' && !Array.isArray(existing)) {
      Object.keys(existing).forEach(function (k) {
        if (typeof existing[k] === 'string') entries[k] = existing[k];
      });
    } else if (typeof existing === 'string') {
      entries.en = existing;
    } else {
      entries.en = '';
    }
    if (entries.en === undefined) entries.en = '';

    function addLocaleRow(locale, value) {
      if (wrap.querySelector('.i18n-cell__locale-row[data-locale="' + locale + '"]')) return;
      var row = document.createElement('div');
      row.className = 'i18n-cell__locale-row';
      row.dataset.locale = locale;
      var lbl = document.createElement('span');
      lbl.className = 'i18n-cell__locale-label';
      lbl.textContent = locale;
      row.appendChild(lbl);
      var inp = document.createElement('input');
      inp.type = 'text';
      inp.placeholder = c.placeholder || c.default_value || '';
      if (value != null) inp.value = String(value);
      row.appendChild(inp);
      wrap.appendChild(row);
    }
    function removeLocaleRow(locale) {
      if (locale === 'en') return;
      var row = wrap.querySelector('.i18n-cell__locale-row[data-locale="' + locale + '"]');
      if (row && row.parentNode) row.parentNode.removeChild(row);
    }

    // Expose for the row-level toolbar to drive.
    wrap._addLocaleRow = addLocaleRow;
    wrap._removeLocaleRow = removeLocaleRow;
    wrap._listLocales = function () {
      return Array.prototype.map.call(
        wrap.querySelectorAll('.i18n-cell__locale-row'),
        function (r) { return r.getAttribute('data-locale'); }
      );
    };

    addLocaleRow('en', entries.en);
    Object.keys(entries).forEach(function (k) {
      if (k === 'en') return;
      addLocaleRow(k, entries[k]);
    });
    td.appendChild(wrap);
  }

  /// Build the per-row language toolbar that drives every multilingual cell
  /// in `tr` simultaneously. Renders one chip per active extra locale (EN
  /// is the always-on baseline and not chip-rendered). Picking a locale +
  /// "+ Add" appends a sub-row in every multilingual cell at once; clicking
  /// a chip's ✕ removes it from every multilingual cell at once.
  function buildLangToolbar(tr, q, columnCount) {
    var multilingualCols = q.list_columns.filter(function (c) { return !!c.multilingual; });
    if (multilingualCols.length === 0) return null;

    var langTr = document.createElement('tr');
    langTr.className = 'row-table__lang-row';
    var langTd = document.createElement('td');
    langTd.className = 'row-table__lang-toolbar';
    langTd.colSpan = columnCount;

    var hint = document.createElement('span');
    hint.className = 'lang-toolbar__hint';
    hint.textContent = 'Languages:';
    langTd.appendChild(hint);

    var enChip = document.createElement('span');
    enChip.className = 'lang-toolbar__chip lang-toolbar__chip--baseline';
    enChip.textContent = 'EN';
    enChip.title = 'English (baseline, always present)';
    langTd.appendChild(enChip);

    var chipsHost = document.createElement('span');
    chipsHost.className = 'lang-toolbar__chips';
    langTd.appendChild(chipsHost);

    var sel = document.createElement('select');
    sel.className = 'lang-toolbar__picker';
    var optBlank = document.createElement('option');
    optBlank.value = '';
    optBlank.textContent = '+ language';
    sel.appendChild(optBlank);
    I18N_LOCALES.forEach(function (l) {
      if (l[0] === 'en') return;
      var o = document.createElement('option');
      o.value = l[0];
      o.textContent = l[0] + ' — ' + l[1];
      sel.appendChild(o);
    });
    langTd.appendChild(sel);

    function rowI18nCells() {
      return tr.querySelectorAll('.i18n-cell');
    }
    function syncOptionsFromActive(active) {
      Array.prototype.forEach.call(sel.options, function (o) {
        o.disabled = active.indexOf(o.value) >= 0 && o.value !== '';
      });
    }
    function activeLocales() {
      var set = {};
      rowI18nCells().forEach(function (cell) {
        cell._listLocales().forEach(function (l) { if (l !== 'en') set[l] = true; });
      });
      return Object.keys(set);
    }
    function renderChip(locale) {
      var chip = document.createElement('span');
      chip.className = 'lang-toolbar__chip';
      chip.dataset.locale = locale;
      chip.textContent = locale;
      var rm = document.createElement('button');
      rm.type = 'button';
      rm.className = 'lang-toolbar__chip-remove';
      rm.title = 'Remove ' + locale;
      rm.textContent = '✕';
      rm.addEventListener('click', function () {
        rowI18nCells().forEach(function (cell) { cell._removeLocaleRow(locale); });
        if (chip.parentNode) chip.parentNode.removeChild(chip);
        syncOptionsFromActive(activeLocales());
      });
      chip.appendChild(rm);
      chipsHost.appendChild(chip);
    }
    function addLocale(locale) {
      if (!locale || locale === 'en') return;
      if (chipsHost.querySelector('.lang-toolbar__chip[data-locale="' + locale + '"]')) return;
      rowI18nCells().forEach(function (cell) { cell._addLocaleRow(locale, ''); });
      renderChip(locale);
      syncOptionsFromActive(activeLocales());
    }

    var addBtn = document.createElement('button');
    addBtn.type = 'button';
    addBtn.className = 'lang-toolbar__add';
    addBtn.textContent = '+ Add';
    addBtn.addEventListener('click', function () {
      var locale = sel.value;
      if (!locale) return;
      addLocale(locale);
      sel.value = '';
    });
    langTd.appendChild(addBtn);

    langTr.appendChild(langTd);
    // Hydrate chips from any locales already seeded in the cells (saved_rows
    // path). Also backfill empty sub-rows so every multilingual cell has the
    // same locale set — keeps the row consistent and the chip-driven removal
    // applies symmetrically across cells.
    var initialActive = activeLocales();
    initialActive.forEach(function (locale) {
      rowI18nCells().forEach(function (cell) { cell._addLocaleRow(locale, ''); });
      renderChip(locale);
    });
    syncOptionsFromActive(initialActive);
    return langTr;
  }

  /// Append one editable row to a `kind: List` table. `values` is an
  /// optional map of column id → cached value to pre-fill (used during
  /// restore). The trash button removes the row.
  function appendTableRow(wrap, q, values) {
    var tbody = wrap.querySelector('[data-rows]');
    if (!tbody) return;
    var tr = document.createElement('tr');
    tr.className = 'row-table__row';
    q.list_columns.forEach(function (c) {
      var td = document.createElement('td');
      td.dataset.col = c.id;
      if (c.multilingual) td.dataset.multilingual = 'true';
      if (c.kind === 'Boolean') td.dataset.boolean = 'true';
      var existing = values && values[c.id];
      if (c.multilingual) {
        buildI18nCell(td, c, existing);
      } else if (c.kind === 'Boolean') {
        var sw = document.createElement('label');
        sw.className = 'switch';
        sw.innerHTML = '<input type="checkbox" data-col="' + esc(c.id) + '" /><span class="switch-slider"></span>';
        if (existing === true || existing === 'true') sw.querySelector('input').checked = true;
        td.appendChild(sw);
      } else if (c.choices && c.choices.length > 0) {
        var sel = document.createElement('select');
        sel.dataset.col = c.id;
        c.choices.forEach(function (opt) {
          var o = document.createElement('option');
          o.value = opt; o.textContent = opt;
          if (existing === opt) o.selected = true;
          sel.appendChild(o);
        });
        td.appendChild(sel);
      } else {
        var inp = document.createElement('input');
        inp.type = 'text';
        inp.dataset.col = c.id;
        inp.placeholder = c.placeholder || c.default_value || '';
        if (existing != null) inp.value = String(existing);
        td.appendChild(inp);
      }
      tr.appendChild(td);
    });
    var actionTd = document.createElement('td');
    actionTd.className = 'row-table__action';
    var rm = document.createElement('button');
    rm.type = 'button';
    rm.className = 'row-table__remove';
    rm.title = 'Remove row';
    rm.textContent = '✕';
    actionTd.appendChild(rm);
    tr.appendChild(actionTd);
    tbody.appendChild(tr);

    // Append a sibling row carrying the row-level language toolbar, so
    // every multilingual cell in `tr` shares one "+ language" picker.
    var langTr = buildLangToolbar(tr, q, q.list_columns.length + 1);
    if (langTr) tbody.appendChild(langTr);

    rm.addEventListener('click', function () {
      if (langTr && langTr.parentNode) langTr.parentNode.removeChild(langTr);
      if (tr.parentNode) tr.parentNode.removeChild(tr);
    });
  }

  /// Wire up the "+ Add row" buttons and (when restoring) seed any
  /// pre-existing rows. Called once after render.
  function setupTableQuestions(questions) {
    var scope = cs();
    var store = state.phase === "shared" ? scope.sharedAnswers :
      (scope.answers[state.providers[state.currentProvider].provider_id] || {});
    questions.forEach(function (q) {
      if (q.kind !== 'List' || !q.list_columns || q.list_columns.length === 0) return;
      var wrap = document.getElementById('t-' + q.id);
      if (!wrap) return;
      var addBtn = wrap.querySelector('[data-add-row]');
      addBtn.addEventListener('click', function () {
        var max = q.max_rows;
        var rowCount = wrap.querySelectorAll('tbody tr').length;
        if (max != null && rowCount >= max) {
          addBtn.disabled = true;
          return;
        }
        appendTableRow(wrap, q, null);
      });
      // Restore previously saved rows. In-memory wizard state takes
      // precedence (so navigating back/forward keeps edits). Otherwise
      // fall back to `q.saved_rows` which the server hydrates from the
      // bundle's persisted tenant config (e.g. nav_links from tenant.json).
      var saved = store[q.id];
      if (!Array.isArray(saved) && Array.isArray(q.saved_rows)) {
        saved = q.saved_rows;
      }
      if (Array.isArray(saved) && saved.length > 0) {
        saved.forEach(function (row) { appendTableRow(wrap, q, row); });
      }
    });
  }

  function restoreFormValues(questions) {
    var scope = cs();
    var store = state.phase === "shared" ? scope.sharedAnswers :
      (scope.answers[state.providers[state.currentProvider].provider_id] || {});
    questions.forEach(function (q) {
      // List/table questions are seeded by setupTableQuestions; skip here.
      if (q.kind === "List") return;
      var el = document.getElementById("f-" + q.id);
      if (!el) return;
      var val = store[q.id];
      var effective = val !== undefined ? val : (q.saved_value || undefined);
      if (effective !== undefined) {
        if (q.kind === "Boolean") { el.checked = effective === true || effective === "true"; }
        else { el.value = effective; }
      } else if (q.default_value && q.kind !== "Boolean") { el.value = q.default_value; }
      else if (q.kind === "Boolean" && q.default_value) { el.checked = q.default_value === "true" || q.default_value === true; }
    });
  }

  function collectFormValues(questions) {
    var scope = cs();
    var store = state.phase === "shared" ? scope.sharedAnswers :
      (scope.answers[state.providers[state.currentProvider].provider_id] || {});
    questions.forEach(function (q) {
      if (q.kind === "List" && q.list_columns && q.list_columns.length > 0) {
        var wrap = document.getElementById("t-" + q.id);
        if (!wrap) return;
        var rows = [];
        wrap.querySelectorAll('tbody tr.row-table__row').forEach(function (tr) {
          var rowObj = {};
          q.list_columns.forEach(function (c) {
            // Multilingual cell: gather per-locale inputs into either a
            // plain string (single locale) or a locale-keyed object.
            if (c.multilingual) {
              var i18nWrap = tr.querySelector('.i18n-cell[data-col="' + c.id + '"]');
              if (!i18nWrap) return;
              var localeRows = i18nWrap.querySelectorAll('.i18n-cell__locale-row');
              var bag = {};
              localeRows.forEach(function (r) {
                var locale = r.getAttribute('data-locale');
                var inp = r.querySelector('input[type="text"]');
                if (!inp) return;
                var v = (inp.value || '').trim();
                if (v) bag[locale] = v;
              });
              var keys = Object.keys(bag);
              if (keys.length === 0) {
                // empty — skip
              } else if (keys.length === 1 && keys[0] === 'en') {
                rowObj[c.id] = bag.en;
              } else {
                rowObj[c.id] = bag;
              }
              return;
            }
            // Query INPUT / SELECT specifically — `<td>` itself also carries
            // `data-col` (for CSS column targeting), so a bare `[data-col]`
            // selector matches the parent cell first and `cell.value` reads
            // back undefined, silently dropping every scalar/Boolean column
            // and (because URL is required) the entire row.
            var cell = tr.querySelector(
              'input[data-col="' + c.id + '"], select[data-col="' + c.id + '"]'
            );
            if (!cell) return;
            if (c.kind === 'Boolean') {
              if (cell.checked) rowObj[c.id] = true;
            } else {
              var v = (cell.value || '').trim();
              if (v) rowObj[c.id] = v;
            }
          });
          // Drop rows where every required column is empty — same rule as
          // the CLI prompt (lets the user add a placeholder row and step
          // out without filling it).
          var hasRequired = q.list_columns.every(function (c) {
            return !c.required || (rowObj[c.id] != null && rowObj[c.id] !== '');
          });
          if (hasRequired && Object.keys(rowObj).length > 0) rows.push(rowObj);
        });
        if (rows.length > 0) store[q.id] = rows;
        else delete store[q.id];
        // Drop the legacy `<id>_json` advanced-input string if it leaked in
        // from a prior setup run's setup-answers.json prefill — without this
        // the ghost key wins over the new array key in tenant.json sync and
        // the wizard's table edits silently disappear.
        var legacyKey = q.id + '_json';
        if (legacyKey in store) delete store[legacyKey];
        return;
      }
      var el = document.getElementById("f-" + q.id);
      if (!el) return;
      var fieldDiv = document.getElementById("field-" + q.id);
      var isHidden = fieldDiv && fieldDiv.style.display === "none";
      if (q.kind === "Boolean") { store[q.id] = el.checked ? "true" : "false"; }
      else if (!isHidden) { var val = el.value.trim(); if (val) store[q.id] = val; }
    });
    if (state.phase === "shared") { scope.sharedAnswers = store; }
    else { scope.answers[state.providers[state.currentProvider].provider_id] = store; }
  }

  function setupVisibility(questions) {
    var depFields = {};
    questions.forEach(function (q) { if (q.visible_if) depFields[q.visible_if.field] = true; });
    Object.keys(depFields).forEach(function (fid) {
      var el = document.getElementById("f-" + fid);
      if (el) { var h = function () { evaluateVisibility(); }; el.addEventListener("change", h); el.addEventListener("input", h); }
    });
    evaluateVisibility();
  }

  function evaluateVisibility() {
    app.querySelectorAll("[data-vis-field]").forEach(function (group) {
      var field = group.getAttribute("data-vis-field");
      var eqVal = group.getAttribute("data-vis-eq");
      var el = document.getElementById("f-" + field);
      if (!el) { group.style.display = "none"; return; }
      var current = el.type === "checkbox" ? (el.checked ? "true" : "false") : el.value;
      group.style.display = current === eqVal ? "" : "none";
    });
  }

  function validateForm(questions) {
    clearErrors();
    var firstErr = null;
    questions.forEach(function (q) {
      if (q.kind === "Boolean") return;
      var group = document.getElementById("field-" + q.id);
      if (group && group.style.display === "none") return;
      var el = document.getElementById("f-" + q.id);
      if (!el) return;
      if (q.required && !el.value.trim() && !q.default_value) {
        showFieldError(el, t("ui.field.required", [q.title]));
        if (!firstErr) firstErr = el;
      }
    });
    if (firstErr) { firstErr.focus(); return false; }
    return true;
  }

  function clearErrors() {
    app.querySelectorAll(".field-error").forEach(function (e) { e.remove(); });
    app.querySelectorAll(".input-error").forEach(function (e) { e.classList.remove("input-error"); });
  }

  function showFieldError(el, msg) {
    el.classList.add("input-error");
    var err = document.createElement("p");
    err.className = "field-error";
    err.textContent = msg;
    el.parentElement.appendChild(err);
  }

  // ── Shared questions ──

  function submitShared() {
    var scope = cs();
    scope.sharedAnswersDone = true;
    state.providers.forEach(function (p) {
      var store = scope.answers[p.provider_id] || {};
      Object.keys(scope.sharedAnswers).forEach(function (k) {
        store[k] = scope.sharedAnswers[k];
      });
      scope.answers[p.provider_id] = store;
    });
    state.currentProvider = 0;
    state.phase = "provider-form";
    render();
  }

  // ── Per-provider form ──

  function renderProviderForm() {
    var scope = cs();
    var p = state.providers[state.currentProvider];
    var form = state.providerForms[p.provider_id];
    if (!providerEnabled(scope, p.provider_id)) {
      renderDisabledProvider(p);
      return;
    }
    if (p.setup_web_component) {
      renderProviderWebComponent(p);
      return;
    }
    if (!form || form.questions.length === 0) {
      if (providerSetupPhaseActions(p).length > 0) {
        state.phase = "provider-setup-action";
        render();
        return;
      }
      completeProviderAndAdvance(p.provider_id);
      return;
    }
    if (providerNeedsGeneratedPublicBaseUrl(scope, p, form)) {
      var publicUrlStatus = scope.publicBaseUrlStatus[p.provider_id] || {};
      if (!publicUrlStatus.failed) {
        if (!publicUrlStatus.running) ensureProviderPublicBaseUrl(p);
        renderProviderPublicBaseUrlLoading(p);
        return;
      }
    }
    var backFn = function () {
      if (state.currentProvider > 0) { state.currentProvider--; state.phase = "provider-form"; }
      else if (state.sharedQuestions.length > 0) { state.phase = "shared"; }
      else { state.phase = "providers"; }
      render();
    };
    renderForm(form.questions, form.title || formatProviderName(p), t("ui.provider.configure", [formatProviderName(p)]), null, function () {
      if (providerSetupPhaseActions(p).length > 0) {
        persistDraftNow().finally(function () {
          state.phase = "provider-setup-action";
          render();
        });
      } else {
        completeProviderAndAdvance(p.provider_id);
      }
    }, backFn);
  }

  function providerNeedsGeneratedPublicBaseUrl(scope, provider, form) {
    if (!form || !Array.isArray(form.questions)) return false;
    if (!scope || !scope.tunnel || scope.tunnel === "off") return false;
    var answers = scope.answers[provider.provider_id] || {};
    var current = String(answers.public_base_url || "").trim();
    if (current) return false;
    return form.questions.some(function (q) {
      return q && q.id === "public_base_url" && q.required === true;
    });
  }

  function ensureProviderPublicBaseUrl(provider) {
    var scope = cs();
    scope.publicBaseUrlStatus[provider.provider_id] = { running: true };
    fetch("/api/setup-public-url", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        tenant: scope.tenant,
        team: scope.team || null,
        env: scope.env,
        tunnel: scope.tunnel || null
      })
    })
    .then(function (r) { return r.json(); })
    .then(function (result) {
      if (!result.ok || !result.public_base_url) {
        scope.publicBaseUrlStatus[provider.provider_id] = {
          failed: true,
          error: result.error || "Could not start setup tunnel"
        };
        render();
        return;
      }
      var store = scope.answers[provider.provider_id] || {};
      store.public_base_url = result.public_base_url;
      scope.answers[provider.provider_id] = store;
      scope.publicBaseUrlStatus[provider.provider_id] = { ok: true };
      persistDraftNow().finally(render);
    })
    .catch(function (err) {
      scope.publicBaseUrlStatus[provider.provider_id] = { failed: true, error: err.message };
      render();
    });
  }

  function renderProviderPublicBaseUrlLoading(provider) {
    var scope = cs();
    var status = scope.publicBaseUrlStatus[provider.provider_id] || {};
    var html =
      '<div class="fade-in center-msg">' +
        '<p class="executing-text">Starting setup tunnel...</p>' +
        '<p class="executing-sub">' + esc(status.error || "Preparing a public callback URL.") + '</p>' +
      '</div>';
    app.innerHTML = html;
  }

  function providerSetupPhaseActions(provider) {
    var descriptor = provider && provider.setup_actions;
    var actions = descriptor && Array.isArray(descriptor.actions) ? descriptor.actions : [];
    return actions.filter(function (action) {
      return action && action.kind === "oauth_install_button" && action.registration;
    });
  }

  function completeProviderAndAdvance(providerId) {
    var scope = cs();
    scope.providersDone[providerId] = true;
    persistDraftNow().finally(advanceProvider);
  }

  function renderProviderSetupAction() {
    var scope = cs();
    var p = state.providers[state.currentProvider];
    var actions = providerSetupPhaseActions(p);
    if (!actions.length) {
      completeProviderAndAdvance(p.provider_id);
      return;
    }
    var status = scope.providerSetupStatus[p.provider_id] || {};
    var completed = status && status.ok === true;
    var running = status && status.running;
    var error = status && status.error;
    var canContinue = completed || !!error;
    var name = formatProviderName(p);
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(name) + '</h2>' +
            '<p class="card-desc">Create or update the provider app using the configuration you entered.</p>' +
          '</div>' +
          '<div class="card-content">';
    actions.forEach(function (action, idx) {
      html += '<div class="setup-action">';
      html += '<div class="setup-action-heading">' + esc(action.label || "Create app") + '</div>';
      if (action.description) html += '<p class="setup-action-desc">' + esc(action.description) + '</p>';
      if (action.kind === "oauth_install_button" || action.registration) {
        html += '<div class="setup-action-warning">⚠️ Event Subscriptions are REQUIRED. After this step the app’s Event Subscriptions Request URL must be <strong>Verified &amp; Saved</strong> (pointing at your public tunnel), and <code>message.im</code> / <code>app_mention</code> subscribed — otherwise the bot receives nothing. If your tunnel URL changes, re-save it.</div>';
      }
      if (completed) {
        html += '<div class="setup-action-status">App setup complete. Its Event Subscriptions &amp; Interactivity request URLs are now linked to this bundle’s message endpoint — send your bot a message to test. If the bundle’s public URL later changes, re-run this step to update them.</div>';
      } else if (running) {
        html += '<div class="setup-action-status">Working...</div>';
      } else {
        html += '<button class="btn btn-primary setup-action-btn btn-run-provider-setup-action" data-action-idx="' + idx + '">' + esc(action.label || "Create app") + '</button>';
      }
      if (error) html += '<div class="setup-action-error">' + esc(error) + '</div>';
      html += '</div>';
    });
    html +=
          '</div>' +
          '<div class="card-footer card-footer-split">' +
            '<button class="btn-secondary" id="btn-prev">' + esc(t("ui.back")) + '</button>' +
            '<button class="btn btn-primary" id="btn-submit"' + (canContinue ? '' : ' disabled') + '>' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';
    app.innerHTML = html;
    var backFn = function () {
      if (providerHasFormQuestions(p)) {
        state.phase = "provider-form";
      } else if (state.currentProvider > 0) {
        state.currentProvider--;
        state.phase = "provider-form";
      } else if (scope.sharedQuestions.length > 0) {
        state.phase = "shared";
      } else {
        state.phase = "providers";
      }
      render();
    };
    document.getElementById("btn-back").addEventListener("click", backFn);
    document.getElementById("btn-prev").addEventListener("click", backFn);
    document.getElementById("btn-submit").addEventListener("click", function () {
      completeProviderAndAdvance(p.provider_id);
    });
    Array.prototype.forEach.call(document.querySelectorAll(".btn-run-provider-setup-action"), function (btn) {
      btn.addEventListener("click", function () {
        runProviderSetupAction(p, actions[Number(btn.getAttribute("data-action-idx"))]);
      });
    });
  }

  function providerHasFormQuestions(provider) {
    var scope = cs();
    var form = scope.providerForms[provider.provider_id];
    return !!(form && Array.isArray(form.questions) && form.questions.length > 0);
  }

  function runProviderSetupAction(provider, action) {
    var scope = cs();
    var popup = openProviderInstallWindow();
    scope.providerSetupStatus[provider.provider_id] = { running: true };
    render();
    fetch("/api/setup-action", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        provider_id: provider.provider_id,
        action_id: action.id,
        answers: scope.answers,
        tenant: scope.tenant,
        team: scope.team || null,
        env: scope.env,
        tunnel: scope.tunnel || null
      })
    })
    .then(function (r) { return r.json(); })
    .then(function (result) {
      if (!result.ok) {
        closeProviderInstallWindow(popup);
        scope.providerSetupStatus[provider.provider_id] = {
          ok: false,
          error: result.error || "Setup action failed"
        };
        render();
        return;
      }
      scope.providerSetupStatus[provider.provider_id] = result;
      if (result.values && typeof result.values === "object") {
        scope.answers[provider.provider_id] = Object.assign({}, scope.answers[provider.provider_id] || {}, result.values);
      }
      var installUrl = setupActionReturnedFinalUrl(provider, result);
      if (installUrl) {
        navigateProviderInstallWindow(popup, installUrl);
      } else {
        closeProviderInstallWindow(popup);
      }
      render();
    })
    .catch(function (err) {
      closeProviderInstallWindow(popup);
      scope.providerSetupStatus[provider.provider_id] = { ok: false, error: err.message };
      render();
    });
  }

  function openProviderInstallWindow() {
    try {
      var popup = window.open("", "_blank");
      if (popup && popup.document) {
        popup.document.write("<!doctype html><title>Working...</title><body style=\"font-family:system-ui,sans-serif;margin:2rem;color:#111\">Working...</body>");
        popup.document.close();
      }
      return popup;
    } catch (e) {
      return null;
    }
  }

  function navigateProviderInstallWindow(popup, url) {
    try {
      if (popup && !popup.closed) {
        popup.opener = null;
        popup.location.href = url;
        return;
      }
    } catch (e) {}
    window.open(url, "_blank", "noopener,noreferrer");
  }

  function closeProviderInstallWindow(popup) {
    try {
      if (popup && !popup.closed) popup.close();
    } catch (e) {}
  }

  function setupActionReturnedFinalUrl(provider, result) {
    var descriptor = provider && provider.setup_actions;
    var actions = descriptor && Array.isArray(descriptor.actions) ? descriptor.actions : [];
    if (!actions.length || !result) return "";
    var context = {};
    mergeFinalSetupActionContext(context, result.values);
    mergeFinalSetupActionContext(context, result.state && result.state.values);
    mergeFinalSetupActionContext(context, result.state);
    mergeFinalSetupActionContext(context, result.setup_status);
    context.setup_status = result.setup_status || (result.state && result.state.setup_status) || {};
    context.values = result.values || (result.state && result.state.values) || {};
    context.provider_id = provider.provider_id;
    for (var i = 0; i < actions.length; i++) {
      var item = resolveFinalSetupAction(provider, actions[i], context);
      if (item && item.url) return item.url;
    }
    var directUrl = deepValue(context, "oauth_authorize_url") || deepValue(context, "authorize_url");
    if (directUrl) return String(directUrl);
    return "";
  }

  function renderProviderWebComponent(p) {
    var scope = cs();
    var descriptor = p.setup_web_component || {};
    var name = formatProviderName(p);
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(name) + '</h2>' +
            '<p class="card-desc" id="setup-component-status">Loading provider setup...</p>' +
          '</div>' +
          '<div class="card-content">' +
            '<div id="setup-component-blocked" class="setup-component-blocked" hidden></div>' +
            '<div id="setup-component-host" class="setup-component-host"></div>' +
            '<details class="setup-diagnostics" id="setup-diagnostics">' +
              '<summary>Advanced diagnostics</summary>' +
              '<div class="setup-diagnostics-list" id="setup-diagnostics-list">No diagnostics yet.</div>' +
            '</details>' +
          '</div>' +
          '<div class="card-footer card-footer-split">' +
            '<button class="btn-secondary" id="btn-prev">' + esc(t("ui.back")) + '</button>' +
            '<button class="btn btn-primary" id="btn-submit" disabled>' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';
    app.innerHTML = html;

    var backFn = function () {
      if (state.currentProvider > 0) { state.currentProvider--; state.phase = "provider-form"; }
      else if (state.sharedQuestions.length > 0) { state.phase = "shared"; }
      else { state.phase = "providers"; }
      render();
    };
    document.getElementById("btn-back").addEventListener("click", backFn);
    document.getElementById("btn-prev").addEventListener("click", backFn);
    document.getElementById("btn-submit").addEventListener("click", function () {
      var existing = scope.providerSetupStatus[p.provider_id] || {};
      scope.providersDone[p.provider_id] = true;
      scope.providerSetupStatus[p.provider_id] = Object.assign({}, existing, {
        mode: "web_component",
        complete: existing.complete === true,
        continued: true
      });
      persistDraftNow().finally(advanceProvider);
    });

    mountProviderWebComponent(p, descriptor, scope);
  }

  function mountProviderWebComponent(p, descriptor, scope) {
    var status = document.getElementById("setup-component-status");
    var blocked = document.getElementById("setup-component-blocked");
    var host = document.getElementById("setup-component-host");
    var submit = document.getElementById("btn-submit");
    var diagnosticsList = document.getElementById("setup-diagnostics-list");
    var providerId = p.provider_id;
    var context = {
      tenant: scope.tenant || "demo",
      locale: currentLocale || "en"
    };
    var moduleUrl = replaceKnownPlaceholders(String(descriptor.module_url || ""), context);
    var tagName = String(descriptor.tag_name || "");
    if (!isSafeSameOriginModuleUrl(moduleUrl) || !tagName) {
      setComponentBlocked(blocked, status, "Provider setup component is not trusted", moduleUrl);
      return;
    }

    import(moduleUrl)
      .then(function () {
        var element = document.createElement(tagName);
        var attributes = descriptor.attributes || {};
        var resolvedAttributes = {};
        Object.keys(attributes).forEach(function (name) {
          resolvedAttributes[name] = replaceKnownPlaceholders(String(attributes[name]), context);
          element.setAttribute(name, resolvedAttributes[name]);
        });

        var completion = descriptor.completion || {};
        var completionEvent = completion.event || (descriptor.events && descriptor.events.complete) || "greentic-provider-setup-complete";
        var stateEvent = completion.state_event || (descriptor.events && descriptor.events.state) || "greentic-provider-setup-state";
        var errorEvent = descriptor.events && descriptor.events.error;
        var statePath = completion.state_path || "setup_status.ok";
        var expected = completion.equals === undefined ? true : completion.equals;
        installProviderSetupEventLogging(providerId, scope, descriptor, diagnosticsList, blocked, status, resolvedAttributes["state-path"] || moduleUrl);
        loadProviderSetupDiagnostics(providerId, scope, diagnosticsList);

        function markComplete(detail) {
          if (!detailMatchesProvider(detail, providerId)) return;
          var publicState = detail && (detail.state || detail.setup_state || detail);
          scope.providersDone[providerId] = true;
          scope.providerSetupStatus[providerId] = {
            mode: "web_component",
            complete: true,
            state: publicState || {},
            setup_status: publicState && publicState.setup_status ? publicState.setup_status : null,
            values: publicState && publicState.values ? publicState.values : null
          };
          status.textContent = "Provider setup complete.";
          submit.disabled = false;
          scheduleDraftSave();
        }

        function markCanContinue(detail) {
          if (!detailMatchesProvider(detail, providerId)) return;
          var publicState = detail && (detail.state || detail.setup_state || detail);
          scope.providerSetupStatus[providerId] = {
            mode: "web_component",
            complete: false,
            state: publicState || {},
            setup_status: publicState && publicState.setup_status ? publicState.setup_status : null,
            values: publicState && publicState.values ? publicState.values : null
          };
          submit.disabled = false;
          scheduleDraftSave();
        }

        function handleState(event) {
          var detail = event.detail || {};
          if (!detailMatchesProvider(detail, providerId)) return;
          status.textContent = "Provider setup service connected.";
          var state = detail.state || detail;
          if (deepValue(state, statePath) === expected) {
            markComplete(detail);
          } else {
            markCanContinue(detail);
          }
        }

        function handleComplete(event) {
          markComplete(event.detail || {});
        }

        document.addEventListener(stateEvent, handleState);
        document.addEventListener(completionEvent, handleComplete);
        if (errorEvent) {
          document.addEventListener(errorEvent, function (event) {
            var detail = event.detail || {};
            if (!detailMatchesProvider(detail, providerId)) return;
            setComponentBlocked(blocked, status, detail.error || "Provider setup service error", resolvedAttributes["state-path"] || moduleUrl, latestProviderSetupDiagnosticText(providerId, detail));
          });
        }
        host.innerHTML = "";
        host.appendChild(element);
        status.textContent = "Provider setup component loaded.";
        return preflightProviderSetupService(resolvedAttributes).then(function () {
          status.textContent = "Provider setup service connected.";
        });
      })
      .catch(function (err) {
        setComponentBlocked(blocked, status, "Provider setup service is not running", err.expected || moduleUrl, err.configure || err.detail || err.message);
      });
  }

  function preflightProviderSetupService(attributes) {
    var statePath = attributes["state-path"];
    if (!statePath || !isSafeSameOriginModuleUrl(statePath)) return Promise.resolve();
    return fetch(statePath, { headers: { "Accept": "application/json" } })
      .then(function (res) {
        return res.text().then(function (text) {
          var parsed = null;
          try { parsed = text ? JSON.parse(text) : null; } catch (e) { /* non-json is fine */ }
          var blocked = parsed && (parsed.blocked || (parsed.setup_status && parsed.setup_status.blocked));
          var retryableSetupBlock = blocked && (
            blocked.retryable === true ||
            (blocked.detail && blocked.detail.retryable === true) ||
            (blocked.detail && blocked.detail.waiting === true)
          );
          if (!res.ok || (blocked && !retryableSetupBlock)) {
            var err = new Error((blocked && (blocked.summary || blocked.error || blocked.detail)) || (parsed && (parsed.error || parsed.detail)) || ("HTTP " + res.status));
            err.expected = (blocked && (blocked.expected || blocked.target)) || (parsed && (parsed.expected || parsed.target)) || statePath;
            err.configure = (blocked && blocked.configure) || (parsed && parsed.configure);
            err.detail = (blocked && (blocked.next || blocked.detail || blocked.summary)) || (parsed && parsed.detail);
            throw err;
          }
        });
      });
  }

  function replaceKnownPlaceholders(value, context) {
    return value
      .replace(/\{tenant\}/g, context.tenant)
      .replace(/\{locale\}/g, context.locale);
  }

  function isSafeSameOriginModuleUrl(url) {
    return typeof url === "string" && url.charAt(0) === "/" && url.slice(0, 2) !== "//" && url.indexOf("\\") === -1;
  }

  function detailMatchesProvider(detail, providerId) {
    if (!detail) return false;
    var detailProviderId = detail.providerId || detail.provider_id;
    return !detailProviderId || detailProviderId === providerId;
  }

  function deepValue(value, path) {
    if (!path) return value;
    return path.split(".").reduce(function (current, segment) {
      return current && typeof current === "object" ? current[segment] : undefined;
    }, value);
  }

  function setComponentBlocked(blocked, status, message, moduleUrl, detail) {
    var submit = document.getElementById("btn-submit");
    if (submit) submit.disabled = false;
    status.textContent = message;
    blocked.hidden = false;
    blocked.innerHTML =
      '<div class="blocked-title">' + esc(message) + '</div>' +
      '<div class="blocked-detail">Expected route/module: <code>' + esc(moduleUrl || "") + '</code></div>' +
      (detail ? '<div class="blocked-detail">' + esc(detail) + '</div>' : '');
  }

  function installProviderSetupEventLogging(providerId, scope, descriptor, diagnosticsList, blocked, status, expectedPath) {
    if (providerSetupLogListeners[providerId]) {
      providerSetupLogListeners[providerId].forEach(function (entry) {
        document.removeEventListener(entry.name, entry.handler);
      });
    }
    var names = providerSetupGenericEventNames(descriptor);
    providerSetupLogListeners[providerId] = names.map(function (name) {
      var handler = function (event) {
        var detail = event.detail || {};
        if (!detailMatchesProvider(detail, providerId)) return;
        rememberProviderSetupDiagnostic(providerId, name, detail, diagnosticsList);
        postProviderSetupEvent(providerId, name, detail, scope);
        if (name === "greentic-provider-setup-request-error" || name === "greentic-provider-setup-action-timeout" || name === "greentic-provider-setup-error") {
          setComponentBlocked(
            blocked,
            status,
            detail.error || detail.message || (name === "greentic-provider-setup-action-timeout" ? "Provider setup action timed out" : "Provider setup request failed"),
            expectedPath,
            latestProviderSetupDiagnosticText(providerId, detail)
          );
          submit.disabled = false;
        }
      };
      document.addEventListener(name, handler);
      return { name: name, handler: handler };
    });
  }

  function providerSetupGenericEventNames(descriptor) {
    var seen = {};
    var names = PROVIDER_SETUP_EVENT_NAMES.slice();
    var events = descriptor && descriptor.events || {};
    Object.keys(events).forEach(function (key) {
      if (typeof events[key] === "string" && events[key].indexOf("greentic-provider-setup-") === 0) {
        names.push(events[key]);
      }
    });
    var completion = descriptor && descriptor.completion || {};
    ["event", "state_event"].forEach(function (key) {
      if (typeof completion[key] === "string" && completion[key].indexOf("greentic-provider-setup-") === 0) {
        names.push(completion[key]);
      }
    });
    return names.filter(function (name) {
      if (seen[name]) return false;
      seen[name] = true;
      return true;
    });
  }

  function postProviderSetupEvent(providerId, eventName, detail, scope) {
    var sanitized = sanitizeProviderSetupEventDetail(detail);
    var payload = {
      provider_id: providerId,
      event_name: eventName,
      event_detail: sanitized,
      current_step_id: setupEventField(sanitized, ["currentStepId", "current_step_id", "stepId", "step_id", "step"]),
      current_progress: setupEventField(sanitized, ["currentProgress", "current_progress", "progress"]),
      action_name: setupEventField(sanitized, ["actionName", "action_name", "action", "name"]),
      request_method: setupEventField(sanitized, ["method", "requestMethod", "request_method"]),
      request_path: setupEventField(sanitized, ["path", "requestPath", "request_path", "url"]),
      http_status: setupEventField(sanitized, ["status", "httpStatus", "http_status"]),
      response_body: setupEventField(sanitized, ["responseBody", "response_body", "body", "response"]),
      error: setupEventField(sanitized, ["error", "message", "detail"]),
      correlation_id: setupEventField(sanitized, ["correlationId", "correlation_id", "trace_id", "traceId", "request-id", "client-request-id"]),
      tenant: scope.tenant || "demo",
      team: scope.team || null,
      env: scope.env || "dev",
      setup_session_id: setupSessionId,
      setup_ui_url: window.location.origin
    };
    try {
      fetch("/api/provider-setup-events", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload),
        keepalive: true
      }).catch(function () { /* best-effort diagnostics only */ });
    } catch (e) {
      /* best-effort diagnostics only */
    }
  }

  function sanitizeProviderSetupEventDetail(value) {
    if (Array.isArray(value)) {
      return value.map(sanitizeProviderSetupEventDetail);
    }
    if (value && typeof value === "object") {
      var output = {};
      Object.keys(value).forEach(function (key) {
        var normalized = key.toLowerCase().replace(/[_-]/g, "");
        if (isSecretProviderSetupEventKey(normalized)) {
          output[key] = "[redacted]";
        } else if (normalized === "usercode") {
          output[key] = shortHashMarker(String(value[key] || ""));
        } else {
          output[key] = sanitizeProviderSetupEventDetail(value[key]);
        }
      });
      return output;
    }
    return value;
  }

  function setupEventField(detail, names) {
    for (var i = 0; i < names.length; i++) {
      var value = deepFindField(detail, names[i]);
      if (value !== undefined && value !== null && value !== "") return value;
    }
    return undefined;
  }

  function deepFindField(value, fieldName) {
    if (!value || typeof value !== "object") return undefined;
    if (Object.prototype.hasOwnProperty.call(value, fieldName)) return value[fieldName];
    var normalized = String(fieldName).toLowerCase().replace(/[_-]/g, "");
    var keys = Object.keys(value);
    for (var i = 0; i < keys.length; i++) {
      if (keys[i].toLowerCase().replace(/[_-]/g, "") === normalized) return value[keys[i]];
    }
    for (var j = 0; j < keys.length; j++) {
      var child = value[keys[j]];
      if (child && typeof child === "object") {
        var found = deepFindField(child, fieldName);
        if (found !== undefined) return found;
      }
    }
    return undefined;
  }

  function rememberProviderSetupDiagnostic(providerId, eventName, detail, container) {
    var sanitized = sanitizeProviderSetupEventDetail(detail || {});
    var entry = {
      timestamp: new Date().toISOString(),
      event_name: eventName,
      event_detail: sanitized,
      current_step_id: setupEventField(sanitized, ["currentStepId", "current_step_id", "stepId", "step_id", "step"]),
      current_progress: setupEventField(sanitized, ["currentProgress", "current_progress", "progress"]),
      action_name: setupEventField(sanitized, ["actionName", "action_name", "action", "name"]),
      request_method: setupEventField(sanitized, ["method", "requestMethod", "request_method"]),
      request_path: setupEventField(sanitized, ["path", "requestPath", "request_path", "url"]),
      http_status: setupEventField(sanitized, ["status", "httpStatus", "http_status"]),
      error: setupEventField(sanitized, ["error", "message", "detail"]),
      correlation_id: setupEventField(sanitized, ["correlationId", "correlation_id", "trace_id", "traceId", "request-id", "client-request-id"])
    };
    var list = providerSetupDiagnostics[providerId] || [];
    list.push(entry);
    if (list.length > 10) list = list.slice(list.length - 10);
    providerSetupDiagnostics[providerId] = list;
    renderProviderSetupDiagnostics(providerId, container);
  }

  function loadProviderSetupDiagnostics(providerId, scope, container) {
    if (!container) return;
    var url = "/api/provider-setup-events?provider_id=" + encodeURIComponent(providerId) +
      "&tenant=" + encodeURIComponent(scope.tenant || "demo") +
      "&team=" + encodeURIComponent(scope.team || "default") +
      "&env=" + encodeURIComponent(scope.env || "dev") +
      "&limit=10";
    fetch(url, { headers: { "Accept": "application/json" } })
      .then(function (res) { return res.ok ? res.json() : null; })
      .then(function (data) {
        if (!data || !Array.isArray(data.events)) return;
        providerSetupDiagnostics[providerId] = data.events.slice(-10);
        renderProviderSetupDiagnostics(providerId, container);
      })
      .catch(function () { /* diagnostics are best-effort */ });
  }

  function renderProviderSetupDiagnostics(providerId, container) {
    if (!container) return;
    var list = providerSetupDiagnostics[providerId] || [];
    if (!list.length) {
      container.textContent = "No diagnostics yet.";
      return;
    }
    container.innerHTML = list.slice(-10).reverse().map(function (entry) {
      var detail = entry.event_detail || {};
      var title = entry.event_name || "provider setup event";
      var meta = [
        entry.current_step_id || detail.step || detail.stepId,
        entry.current_progress || detail.progress,
        entry.action_name || detail.action || detail.actionName,
        ((entry.request_method || detail.method || "") + " " + (entry.request_path || detail.path || detail.url || "")).trim(),
        entry.http_status || detail.status || detail.httpStatus,
        entry.correlation_id || detail.correlationId || detail.trace_id
      ].filter(function (part) { return part !== undefined && part !== null && String(part).trim() !== ""; }).join(" | ");
      var error = entry.error || detail.error || detail.message || "";
      return '<div class="setup-diagnostic-entry">' +
        '<div class="setup-diagnostic-title">' + esc(title) + '</div>' +
        '<div class="setup-diagnostic-meta">' + esc(meta || entry.timestamp || "") + '</div>' +
        (error ? '<div class="setup-diagnostic-error">' + esc(String(error)) + '</div>' : '') +
      '</div>';
    }).join("");
  }

  function latestProviderSetupDiagnosticText(providerId, fallbackDetail) {
    var list = providerSetupDiagnostics[providerId] || [];
    var entry = list.length ? list[list.length - 1] : { event_detail: fallbackDetail || {} };
    var detail = entry.event_detail || {};
    return [
      entry.event_name,
      entry.current_step_id || detail.step || detail.stepId,
      ((entry.request_method || detail.method || "") + " " + (entry.request_path || detail.path || detail.url || "")).trim(),
      entry.http_status || detail.status || detail.httpStatus,
      entry.error || detail.error || detail.message || detail.detail
    ].filter(function (part) { return part !== undefined && part !== null && String(part).trim() !== ""; }).join(" | ");
  }

  function isSecretProviderSetupEventKey(normalizedKey) {
    return [
      "accesstoken",
      "refreshtoken",
      "idtoken",
      "clientsecret",
      "botapppassword",
      "devicecode",
      "oauthdevicecode"
    ].indexOf(normalizedKey) !== -1;
  }

  function shortHashMarker(value) {
    if (!value) return "[redacted]";
    var hash = 2166136261;
    for (var i = 0; i < value.length; i++) {
      hash ^= value.charCodeAt(i);
      hash = Math.imul(hash, 16777619);
    }
    return "[hash:" + (hash >>> 0).toString(16).padStart(8, "0") + "]";
  }

  function renderDisabledProvider(p) {
    var scope = cs();
    var backFn = function () {
      if (state.currentProvider > 0) { state.currentProvider--; state.phase = "provider-form"; }
      else if (state.sharedQuestions.length > 0) { state.phase = "shared"; }
      else { state.phase = "providers"; }
      render();
    };
    var name = formatProviderName(p);
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(name) + '</h2>' +
            '<p class="card-desc">This provider is disabled. Its inputs, setup actions, OAuth buttons, and webhook steps will be skipped.</p>' +
          '</div>' +
          '<div class="card-content">' +
            '<div class="field-row provider-disabled-row">' +
              '<label class="field-label" for="provider-enabled-toggle">Enable provider</label>' +
              '<label class="switch"><input type="checkbox" id="provider-enabled-toggle" /><span class="switch-slider"></span></label>' +
            '</div>' +
          '</div>' +
          '<div class="card-footer">' +
            '<button class="btn-secondary" id="btn-prev">' + esc(t("ui.back")) + '</button>' +
            '<button class="btn btn-primary" id="btn-submit">' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';
    app.innerHTML = html;
    document.getElementById("provider-enabled-toggle").addEventListener("change", function (e) {
      setProviderEnabled(scope, p.provider_id, e.target.checked);
      persistDraftNow().finally(function () {
        state.phase = "provider-form";
        render();
      });
    });
    document.getElementById("btn-back").addEventListener("click", backFn);
    document.getElementById("btn-prev").addEventListener("click", backFn);
    document.getElementById("btn-submit").addEventListener("click", function () {
      scope.providersDone[p.provider_id] = true;
      persistDraftNow().finally(advanceProvider);
    });
  }

  function advanceProvider() {
    var nextIdx = state.currentProvider + 1;
    if (nextIdx < state.providers.length) {
      state.currentProvider = nextIdx;
      state.phase = "provider-form";
    } else {
      state.phase = "review";
    }
    render();
  }

  // ── Review ──

  function renderReview() {
    var scope = cs();
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back-review">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(t("ui.review.title")) + '</h2>' +
            '<p class="card-desc">' + esc(t("ui.review.description")) + '</p>' +
          '</div>' +
          '<div class="card-content">';

    // Scope summary
    html +=
      '<div class="review-group"><h4 class="review-group-title">' + esc(t("ui.scope.title")) + '</h4>' +
        '<div class="review-item"><span class="review-key">tenant</span><span class="review-val">' + esc(scope.tenant) + '</span></div>' +
        '<div class="review-item"><span class="review-key">env</span><span class="review-val">' + esc(scope.env) + '</span></div>' +
        '<div class="review-item"><span class="review-key">team</span><span class="review-val">' + esc(scope.team || "default") + '</span></div>' +
      '</div>';

    state.providers.forEach(function (p) {
      var setupStatus = scope.providerSetupStatus && scope.providerSetupStatus[p.provider_id];
      if (setupStatus && setupStatus.mode === "web_component" && setupStatus.complete) {
        html += '<div class="review-group"><h4 class="review-group-title">' + esc(formatProviderName(p)) + '</h4>';
        html += '<div class="review-item"><span class="review-key">setup</span><span class="review-val">web component complete</span></div>';
        html += '</div>';
        return;
      }
      var answers = scope.answers[p.provider_id] || {};
      var form = state.providerForms[p.provider_id];
      var keys = Object.keys(answers);
      if (keys.length === 0) return;
      html += '<div class="review-group"><h4 class="review-group-title">' + esc(formatProviderName(p)) + '</h4>';
      keys.forEach(function (k) {
        var val = answers[k];
        var isSecret = form && form.questions.some(function (q) { return q.id === k && q.secret; });
        var display = typeof val === "boolean" ? (val ? t("ui.review.yes") : t("ui.review.no")) : (isSecret && val ? t("ui.review.secret_mask") : String(val || ""));
        if (!display) return;
        html += '<div class="review-item"><span class="review-key">' + esc(k) + '</span><span class="review-val' + (isSecret ? ' secret' : '') + '">' + esc(display) + '</span></div>';
      });
      html += '</div>';
    });

    html +=
          '</div>' +
          '<div class="card-footer">' +
            '<button class="btn btn-primary btn-lg" id="btn-execute">' +
              '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m5 12 7-7 7 7"/><path d="M12 19V5"/></svg>' +
              ' ' + esc(t("ui.execute_setup")) +
            '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;
    document.getElementById("btn-back-review").addEventListener("click", function () { state.phase = "providers"; render(); });
    document.getElementById("btn-execute").addEventListener("click", executeSetup);
  }

  // ── Execute ──

  function executeSetup() {
    var scope = cs();
    state.phase = "executing";
    render();
    var payload = {
      answers: scope.answers,
      provider_setup_status: scope.providerSetupStatus || {},
      tenant: scope.tenant,
      env: scope.env,
      tunnel: scope.tunnel || null,
    };
    if (scope.team) payload.team = scope.team;
    // Surface table answers in the browser console so we can confirm what's
    // actually being POSTed when the disk doesn't match expectations.
    try {
      Object.keys(scope.answers || {}).forEach(function (pid) {
        var pa = scope.answers[pid];
        if (pa && typeof pa === 'object' && Array.isArray(pa.nav_links)) {
          console.log('[setup] POST /api/execute — ' + pid + ' nav_links:', JSON.parse(JSON.stringify(pa.nav_links)));
        }
      });
    } catch (e) { /* logging is best-effort */ }
    fetch("/api/execute", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    })
    .then(function (r) { return r.json(); })
    .then(function (result) {
      scope.executed = true;
      state.result = result;
      state.phase = "result";
      render();
    })
    .catch(function (err) {
      state.result = { success: false, stdout: "", stderr: err.message };
      state.phase = "result";
      render();
    });
  }

  function renderExecuting() {
    app.innerHTML =
      '<div class="fade-in center-msg">' +
        '<div class="spinner"></div>' +
        '<p class="executing-text">' + esc(t("ui.executing.title")) + '</p>' +
        '<p class="executing-sub">' + esc(t("ui.executing.sub")) + '</p>' +
      '</div>';
  }

  function renderResult() {
    var r = state.result;
    var ok = r && r.success;
    var html =
      '<div class="fade-in"><div class="card">' +
        '<div class="card-header center">' +
          '<div class="result-icon ' + (ok ? "result-ok" : "result-err") + '">' +
            (ok ?
              '<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 11.08V12a10 10 0 1 1-5.93-9.14"/><path d="m9 11 3 3L22 4"/></svg>' :
              '<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/></svg>'
            ) +
          '</div>' +
          '<h2 class="card-title">' + (ok ? esc(t("ui.result.success.title")) : esc(t("ui.result.fail.title"))) + '</h2>' +
          '<p class="card-desc">' + (ok ? esc(t("ui.result.success.description")) : esc(t("ui.result.fail.description"))) + '</p>' +
        '</div>' +
        '<div class="card-content">';

    if (r && r.manual_steps && r.manual_steps.length > 0) {
      html += '<div class="output-section"><h4 class="output-title" style="color:#f59e0b">' + esc(t("ui.result.manual_steps")) + '</h4>';
      r.manual_steps.forEach(function (instr) {
        html += '<div style="margin-bottom:.75rem;padding:.75rem 1rem;background:rgba(245,158,11,.06);border:1px solid rgba(245,158,11,.2);border-radius:calc(var(--radius) - 2px)">';
        html += '<div style="font-size:.8125rem;font-weight:600;color:#f59e0b;margin-bottom:.375rem">' + esc(instr.provider_name) + '</div>';
        html += '<ol style="margin:0;padding-left:1.25rem;font-size:.8rem;color:#d4d4d8;line-height:1.7">';
        instr.steps.forEach(function (step) { html += '<li>' + esc(step.replace(/^\d+\.\s*/, '')) + '</li>'; });
        html += '</ol></div>';
      });
      html += '</div>';
    }
    if (r && r.pending_setup_actions && r.pending_setup_actions.length > 0) {
      html += '<div class="output-section"><h4 class="output-title">Setup actions</h4>';
      r.pending_setup_actions.forEach(function (action, idx) {
        if (action.kind === "oauth_install_button" && action.authorize_url) {
        html += '<div class="setup-action">';
        html += '<a class="btn btn-primary setup-action-btn" target="_blank" rel="noopener noreferrer" href="' + esc(action.authorize_url) + '">' + esc(action.label || "Open") + '</a>';
        html += '</div>';
          return;
        }
        if (action.kind === "oauth_device_code") {
          var report = action.device_report || null;
          var poll = action.device_poll || null;
          var checklist = [].concat(action.checklist || [], report && report.checklist ? report.checklist : [], poll && poll.checklist ? poll.checklist : []);
          var verifyUrl = report && (report.verification_uri_complete || report.verification_uri);
          var beforeText = action.instructions_before_start || action.start_instructions || action.instructions || "";
          var afterSteps = deviceActionInstructionSteps(action, report);
          html += '<div class="setup-action">';
          html += '<div class="setup-action-heading">' + esc(action.label || "Connect") + '</div>';
          if (!report) {
            if (beforeText) html += '<div class="setup-action-help">' + esc(beforeText) + '</div>';
            if (action.device_starting) {
              html += '<div class="setup-action-status">' + esc(action.starting_label || "Starting...") + '</div>';
            } else if (action.device_error || action.auto_start === false) {
              html += '<button class="btn btn-primary setup-action-btn btn-oauth-device-start" data-action-idx="' + idx + '">' + esc(action.start_label || action.label || "Start") + '</button>';
            }
          } else {
            if (afterSteps.length > 0) {
              html += '<ol class="setup-action-steps">';
              afterSteps.forEach(function (step) {
                html += '<li>' + renderDeviceInstructionStep(step, action, report) + '</li>';
              });
              html += '</ol>';
            }
            html += '<div class="setup-action-code-row">';
            html += '<div class="setup-action-code">' + esc(report.user_code) + '</div>';
            html += '<button class="btn btn-secondary btn-oauth-device-copy" data-session-id="' + esc(report.session_id) + '" data-code="' + esc(report.user_code) + '">' + esc(action.copy_label || "Copy code") + '</button>';
            html += '</div>';
            html += '<a class="btn btn-primary setup-action-btn" target="_blank" rel="noopener noreferrer" href="' + esc(verifyUrl) + '">' + esc(deviceActionVerificationLabel(action)) + '</a>';
            html += '<div class="setup-action-url">' + esc(report.verification_uri) + '</div>';
            html += '<button class="btn btn-secondary btn-oauth-device-poll" data-session-id="' + esc(report.session_id) + '">' + esc(deviceActionFinalizeLabel(action)) + '</button>';
          }
          if (checklist.length > 0) {
            html += '<details class="setup-action-details"><summary>' + esc(action.requirements_label || "Provider requirements") + '</summary><ul class="setup-action-checklist">';
            checklist.forEach(function (item) { html += '<li>' + esc(item) + '</li>'; });
            html += '</ul></details>';
          }
          if (action.device_status) {
            html += '<div class="setup-action-status">' + esc(action.device_status) + '</div>';
          }
          if (action.device_error) {
            html += '<div class="setup-action-error">' + esc(action.device_error) + '</div>';
          }
          html += '</div>';
        }
      });
      html += '</div>';
    }
    var finalActions = resolveFinalSetupActions();
    if (finalActions.length > 0) {
      html += renderFinalSetupActions(finalActions);
    }
    if (r && r.stdout) html += '<div class="output-section"><h4 class="output-title">' + esc(t("ui.result.output")) + '</h4><pre class="output-pre">' + esc(r.stdout) + '</pre></div>';
    if (r && r.stderr) html += '<div class="output-section"><h4 class="output-title">' + esc(t("ui.result.log")) + '</h4><pre class="output-pre stderr">' + esc(r.stderr) + '</pre></div>';

    html +=
        '</div>' +
        '<div class="card-footer card-footer-split">' +
          '<button class="btn btn-secondary" id="btn-back-dash">' + esc(t("ui.dashboard.back")) + '</button>' +
          '<button class="btn btn-ghost" id="btn-close">' + esc(t("ui.close")) + '</button>' +
        '</div>' +
      '</div></div>';

    app.innerHTML = html;
    document.getElementById("btn-back-dash").addEventListener("click", function () { state.phase = "dashboard"; render(); });
    document.getElementById("btn-close").addEventListener("click", shutdown);
    Array.prototype.forEach.call(document.querySelectorAll(".btn-oauth-device-start"), function (btn) {
      btn.addEventListener("click", function () { startOAuthDeviceAction(Number(btn.getAttribute("data-action-idx"))); });
    });
    Array.prototype.forEach.call(document.querySelectorAll(".btn-oauth-device-poll"), function (btn) {
      btn.addEventListener("click", function () { pollOAuthDeviceAction(btn.getAttribute("data-session-id")); });
    });
    Array.prototype.forEach.call(document.querySelectorAll(".btn-oauth-device-copy"), function (btn) {
      btn.addEventListener("click", function () { copyOAuthDeviceCode(btn.getAttribute("data-code"), btn.getAttribute("data-session-id")); });
    });
    Array.prototype.forEach.call(document.querySelectorAll(".btn-final-action-copy"), function (btn) {
      btn.addEventListener("click", function () {
        copyFinalSetupActionValue(btn.getAttribute("data-copy-value"), btn);
      });
    });
    autoStartOAuthDeviceActions();
  }

  function resolveFinalSetupActions() {
    var scope = cs() || {};
    var result = state.result || {};
    var setupStatusByProvider = result.provider_setup_status || scope.providerSetupStatus || {};
    var resolved = [];
    state.providers.forEach(function (provider) {
      var descriptor = provider.setup_actions;
      var actions = descriptor && Array.isArray(descriptor.actions) ? descriptor.actions : [];
      var providerState = setupStatusByProvider[provider.provider_id] || {};
      var preResolved = providerState && providerState.state && Array.isArray(providerState.state.final_setup_actions) ? providerState.state.final_setup_actions : [];
      preResolved.forEach(function (action) {
        if (action && action.url && action.label) {
          resolved.push({
            provider_id: action.provider_id || provider.provider_id,
            action_id: action.action_id || action.id || "",
            label: action.label,
            url: action.url,
            html: action.html || finalSetupActionHtml(provider.provider_id, action.action_id || action.id || "add", action.label, action.url)
          });
        }
      });
      if (!actions.length || preResolved.length) return;
      var context = finalSetupActionContext(providerState, result, scope, provider);
      actions.forEach(function (action) {
        var item = resolveFinalSetupAction(provider, action, context);
        if (item) resolved.push(item);
      });
    });
    return resolved;
  }

  function finalSetupActionContext(providerState, result, scope, provider) {
    var context = {};
    mergeFinalSetupActionContext(context, scope && scope.answers && scope.answers[provider.provider_id]);
    mergeFinalSetupActionContext(context, result && result.values);
    mergeFinalSetupActionContext(context, providerState && providerState.values);
    mergeFinalSetupActionContext(context, providerState && providerState.state && providerState.state.values);
    mergeFinalSetupActionContext(context, providerState && providerState.state);
    mergeFinalSetupActionContext(context, providerState && providerState.setup_status);
    context.setup_status = (providerState && providerState.setup_status) || (providerState && providerState.state && providerState.state.setup_status) || {};
    context.values = (providerState && providerState.values) || (providerState && providerState.state && providerState.state.values) || {};
    context.tenant = scope.tenant || "demo";
    context.team = scope.team || "default";
    context.provider_id = provider.provider_id;
    return context;
  }

  function mergeFinalSetupActionContext(target, source) {
    if (!source || typeof source !== "object" || Array.isArray(source)) return;
    Object.keys(source).forEach(function (key) {
      var value = source[key];
      if (value === undefined || value === null) return;
      if (isFinalSetupSecretKey(key)) return;
      if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
        target[key] = String(value);
      } else if (value && typeof value === "object" && !Array.isArray(value)) {
        mergeFinalSetupActionContext(target, value);
      }
    });
  }

  function isFinalSetupSecretKey(key) {
    return /(^|_)(token|secret|password|device_code|refresh_token|access_token|id_token|bot_access_token)($|_)/i.test(String(key || ""));
  }

  function resolveFinalSetupAction(provider, action, context) {
    if (!action) return null;
    if (action.kind !== "deep_link") return null;
    var id = String(action.id || "").trim();
    var label = String(action.label || "").trim();
    var template = String(action.url_template || "").trim();
    if (!id || !label || !template) return null;
    if (!finalSetupActionVisible(action.visible_when, context)) return null;
    var required = Array.isArray(action.requires) ? action.requires : [];
    for (var i = 0; i < required.length; i++) {
      var name = String(required[i] || "");
      var value = deepValue(context, name);
      if (!name || value === undefined || value === null || value === "") return null;
    }
    var wholePlaceholder = template.match(/^\{([A-Za-z0-9_.-]+)\}$/);
    var unresolved = false;
    var url = wholePlaceholder ? String(deepValue(context, wholePlaceholder[1]) || "") : template.replace(/\{([A-Za-z0-9_.-]+)\}/g, function (_, name) {
      var value = deepValue(context, name);
      if (value === undefined || value === null || value === "") {
        unresolved = true;
        return "";
      }
      return encodeURIComponent(String(value));
    });
    if (unresolved || !isSafeFinalSetupActionUrl(url)) return null;
    return {
      provider_id: provider.provider_id,
      action_id: id,
      label: label,
      url: url,
      html: finalSetupActionHtml(provider.provider_id, id, label, url)
    };
  }

  function finalSetupActionVisible(visibleWhen, context) {
    if (!visibleWhen || typeof visibleWhen !== "object" || Array.isArray(visibleWhen)) return true;
    return Object.keys(visibleWhen).every(function (path) {
      return deepValue(context, path) === visibleWhen[path];
    });
  }

  function isSafeFinalSetupActionUrl(url) {
    try {
      var parsed = new URL(url, window.location.href);
      return parsed.protocol === "https:";
    } catch (e) {
      return false;
    }
  }

  function finalSetupActionHtml(providerId, actionId, label, url) {
    var cls = "greentic-add-button " + sanitizeCssClass("greentic-add-" + providerId + "-" + actionId);
    return '<a class="' + escAttr(cls) + '" href="' + escAttr(url) + '" target="_blank" rel="noopener noreferrer">' + esc(label) + '</a>';
  }

  function sanitizeCssClass(value) {
    return String(value || "").toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "greentic-add";
  }

  function renderFinalSetupActions(actions) {
    var html = '<div class="output-section final-setup-actions">' +
      '<h4 class="output-title">Share add buttons</h4>' +
      '<p class="setup-action-help">Share these links directly or add the HTML buttons to an internal page.</p>';
    actions.forEach(function (action) {
      html += '<div class="setup-action final-setup-action">';
      html += '<div class="setup-action-heading">' + esc(action.label) + '</div>';
      html += '<a class="btn btn-primary setup-action-btn" target="_blank" rel="noopener noreferrer" href="' + escAttr(action.url) + '">' + esc(action.label) + '</a>';
      html += '<label class="final-action-field-label">URL</label>';
      html += '<div class="final-action-copy-row"><input class="input final-action-value" readonly value="' + escAttr(action.url) + '"><button class="btn btn-secondary btn-final-action-copy" data-copy-value="' + escAttr(action.url) + '">Copy</button></div>';
      html += '<label class="final-action-field-label">HTML</label>';
      html += '<div class="final-action-copy-row"><input class="input final-action-value" readonly value="' + escAttr(action.html) + '"><button class="btn btn-secondary btn-final-action-copy" data-copy-value="' + escAttr(action.html) + '">Copy</button></div>';
      html += '</div>';
    });
    html += '</div>';
    return html;
  }

  function copyFinalSetupActionValue(value, btn) {
    copyTextToClipboard(value || "").then(function () {
      if (!btn) return;
      var old = btn.textContent;
      btn.textContent = "Copied";
      setTimeout(function () { btn.textContent = old; }, 1200);
    }).catch(function (err) {
      if (btn) btn.textContent = err.message || "Copy failed";
    });
  }

  function startOAuthDeviceAction(idx) {
    var actions = state.result && state.result.pending_setup_actions;
    if (!actions || !actions[idx]) return;
    var action = actions[idx];
    if (action.device_starting) return;
    action.device_starting = true;
    action.device_status = null;
    action.device_error = null;
    render();
    fetch("/api/oauth-device/start", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        provider_id: action.provider_id,
        tenant: action.tenant,
        team: action.team || null,
        action_id: action.id,
      }),
    })
    .then(function (r) { return r.json(); })
    .then(function (res) {
      if (res.ok) {
        action.device_report = res.report;
        action.device_error = null;
        action.device_poll = null;
        action.device_status = action.ready_message || null;
      } else {
        action.device_error = res.error || "Device login failed";
        action.device_status = null;
      }
      action.device_starting = false;
      render();
    })
    .catch(function (err) {
      action.device_error = err.message;
      action.device_status = null;
      action.device_starting = false;
      render();
    });
  }

  function autoStartOAuthDeviceActions() {
    var actions = state.result && state.result.pending_setup_actions;
    if (!actions) return;
    actions.forEach(function (action, idx) {
      if (
        action.kind === "oauth_device_code" &&
        action.auto_start !== false &&
        !action.device_report &&
        !action.device_starting &&
        !action.device_error
      ) {
        setTimeout(function () { startOAuthDeviceAction(idx); }, 0);
      }
    });
  }

  function pollOAuthDeviceAction(sessionId) {
    var action = findOAuthDeviceActionBySession(sessionId);
    if (action) {
      action.device_status = "Checking authorization status...";
      action.device_error = null;
      render();
    }
    fetch("/api/oauth-device/poll", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ session_id: sessionId }),
    })
    .then(function (r) { return r.json(); })
    .then(function (res) {
      action = action || findOAuthDeviceActionBySession(sessionId);
      if (!res.ok) {
        if (action) {
          action.device_error = res.error || "Device login failed";
          action.device_status = null;
          render();
        } else {
          alert(res.error || "Device login failed");
        }
        return;
      }
      var report = res.report || {};
      if (action) {
        action.device_poll = report;
        action.device_error = null;
      }
      if (report.status === "complete") {
        var keys = report.persisted_keys || [];
        var completeMessage = deviceActionCompleteMessage(action, keys);
        state.result.stdout = (state.result.stdout || "") + "\n" + completeMessage;
        state.result.pending_setup_actions = (state.result.pending_setup_actions || []).filter(function (action) {
          return !(action.device_report && action.device_report.session_id === sessionId);
        });
      } else if (action) {
        if (report.status === "pending") {
          action.device_status = report.message || "Authorization is still pending.";
        } else if (report.status === "slow_down") {
          action.device_status = report.message || "The provider asked setup to slow down. Wait a moment, then check again.";
        } else {
          action.device_error = report.message || "Device login failed.";
          action.device_status = null;
        }
      }
      render();
    })
    .catch(function (err) {
      action = action || findOAuthDeviceActionBySession(sessionId);
      if (action) {
        action.device_error = err.message;
        action.device_status = null;
        render();
      } else {
        alert(err.message);
      }
    });
  }

  function findOAuthDeviceActionBySession(sessionId) {
    var actions = state.result && state.result.pending_setup_actions;
    if (!actions) return null;
    for (var i = 0; i < actions.length; i++) {
      if (actions[i].device_report && actions[i].device_report.session_id === sessionId) return actions[i];
    }
    return null;
  }

  function copyOAuthDeviceCode(code, sessionId) {
    if (!code) return;
    var action = findOAuthDeviceActionBySession(sessionId);
    copyTextToClipboard(code).then(function () {
      if (action) {
        action.device_status = action.copy_success_message || "Code copied.";
        action.device_error = null;
        render();
      }
    }).catch(function (err) {
      if (action) {
        action.device_error = err.message || "Could not copy code.";
        render();
      }
    });
  }

  function copyTextToClipboard(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    return new Promise(function (resolve, reject) {
      var input = document.createElement("textarea");
      input.value = text;
      input.setAttribute("readonly", "");
      input.style.position = "fixed";
      input.style.left = "-9999px";
      input.style.top = "0";
      document.body.appendChild(input);
      input.focus();
      input.select();
      try {
        if (document.execCommand("copy")) resolve();
        else reject(new Error("Could not copy code."));
      } catch (err) {
        reject(err);
      } finally {
        document.body.removeChild(input);
      }
    });
  }

  function deviceActionInstructionSteps(action, report) {
    var configured = action.instructions_after_start || action.approval_steps || action.device_code_steps || action.steps;
    if (typeof configured === "string") {
      configured = configured.split(/\r?\n/).map(function (line) { return line.replace(/^\s*\d+[\).\s-]*/, "").trim(); }).filter(Boolean);
    }
    if (Array.isArray(configured) && configured.length > 0) return configured;
    return [];
  }

  function renderDeviceInstructionStep(step, action, report) {
    var text = typeof step === "string" ? step : String(step || "");
    var label = action.label || "Open verification page";
    var verificationLabel = deviceActionVerificationLabel(action);
    var finalizeLabel = deviceActionFinalizeLabel(action);
    return esc(text)
      .replace(/\{user_code\}/g, '<span class="setup-action-code-inline">' + esc(report.user_code) + '</span>')
      .replace(/\{code\}/g, '<span class="setup-action-code-inline">' + esc(report.user_code) + '</span>')
      .replace(/\{label\}/g, '<strong>' + esc(label) + '</strong>')
      .replace(/\{action_label\}/g, '<strong>' + esc(label) + '</strong>')
      .replace(/\{verification_label\}/g, '<strong>' + esc(verificationLabel) + '</strong>')
      .replace(/\{authorize_label\}/g, '<strong>' + esc(verificationLabel) + '</strong>')
      .replace(/\{finalize_label\}/g, '<strong>' + esc(finalizeLabel) + '</strong>')
      .replace(/\{verification_uri\}/g, esc(report.verification_uri || "verification page"))
      .replace(/\{verification_url\}/g, esc(report.verification_uri || "verification page"));
  }

  function deviceActionVerificationLabel(action) {
    return action.verification_label || action.authorize_label || action.open_label || action.label || "Open";
  }

  function deviceActionFinalizeLabel(action) {
    return action.finalize_label || action.complete_label || action.poll_label || action.label || "Check status";
  }

  function deviceActionCompleteMessage(action, keys) {
    var message = action.success_message || action.complete_message || action.verified_message;
    if (!message) {
      message = "OAuth device-code setup complete" + (keys.length ? " ({persisted_keys})" : "") + ".";
    }
    return message
      .replace(/\{persisted_keys\}/g, keys.join(", "))
      .replace(/\{label\}/g, action.label || "")
      .replace(/\{action_label\}/g, action.label || "");
  }

  // ── Export ──

  function renderExport() {
    var html =
      '<div class="fade-in">' +
        '<div class="step-header">' +
          '<button class="btn btn-ghost btn-sm btn-back" id="btn-back-export">' +
            '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg>' +
            ' ' + esc(t("ui.back")) +
          '</button>' +
        '</div>' +
        '<div class="card">' +
          '<div class="card-header">' +
            '<h2 class="card-title">' + esc(t("ui.export.title")) + '</h2>' +
            '<p class="card-desc">' + esc(t("ui.export.description")) + '</p>' +
          '</div>' +
          '<div class="card-content"><div class="form-fields">' +
            '<div class="field">' +
              '<label class="field-label" for="f-export-key">' + esc(t("ui.export.password")) + '</label>' +
              '<input type="password" id="f-export-key" placeholder="' + esc(t("ui.export.password_hint")) + '" />' +
              '<p class="field-help">' + esc(t("ui.export.password_help")) + '</p>' +
            '</div>' +
          '</div></div>' +
          '<div class="card-footer">' +
            '<button class="btn btn-primary btn-lg" id="btn-do-export" style="width:100%">' + esc(t("ui.export.download")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;

    document.getElementById("btn-back-export").addEventListener("click", function () { state.phase = "dashboard"; render(); });

    document.getElementById("btn-do-export").addEventListener("click", function () {
      var key = document.getElementById("f-export-key").value.trim() || null;
      var scopes = state.scopes.map(function (s) {
        return { tenant: s.tenant, team: s.team || null, env: s.env, answers: s.answers };
      });
      fetch("/api/export", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ scopes: scopes, key: key }),
      })
      .then(function (r) { return r.json(); })
      .then(function (doc) {
        var blob = new Blob([JSON.stringify(doc, null, 2)], { type: "application/json" });
        var a = document.createElement("a");
        a.href = URL.createObjectURL(blob);
        a.download = "setup-answers.json";
        a.click();
        URL.revokeObjectURL(a.href);
      })
      .catch(function (err) { alert("Export failed: " + err.message); });
    });
  }

  // ── Helpers ──

  function shutdown() {
    fetch("/api/shutdown", { method: "POST" });
    app.innerHTML = '<div class="fade-in center-msg"><p class="executing-text">' + esc(t("ui.result.closed")) + '</p><p class="executing-sub">' + esc(t("ui.result.closed_sub")) + '</p></div>';
  }

  function esc(str) {
    var d = document.createElement("div");
    d.textContent = str || "";
    return d.innerHTML;
  }

  function escAttr(str) {
    return esc(str);
  }

  function formatProviderName(provider) {
    if (typeof provider === "object" && provider.display_name) return provider.display_name;
    return typeof provider === "object" ? provider.provider_id : provider;
  }

  if (typeof window !== "undefined") {
    window.__greenticSetupTestHooks = {
      providerSetupGenericEventNames: providerSetupGenericEventNames,
      sanitizeProviderSetupEventDetail: sanitizeProviderSetupEventDetail,
      resolveFinalSetupAction: resolveFinalSetupAction,
      finalSetupActionContext: finalSetupActionContext,
      renderFinalSetupActions: renderFinalSetupActions
    };
  }

  render();
})();
