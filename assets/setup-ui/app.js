(function () {
  "use strict";

  var app = document.getElementById("app");
  var i18n = {};  // populated from /api/providers response
  var currentLocale = "en";
  var localeOptions = [];

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
          state.providerForms[pf.provider_id] = pf;
        });
        state.sharedQuestions = data.shared_questions || [];
        render();
      });
  }

  var state = {
    phase: "loading",        // loading | providers | shared | provider-form | review | executing | result
    providers: [],           // [{provider_id, domain, question_count}]
    sharedQuestions: [],      // FormSpec questions
    providerForms: {},        // {provider_id: {questions, title}}
    currentProvider: 0,       // index into providers
    answers: {},              // {provider_id: {field: value}}
    sharedAnswers: {},        // {field: value}
    providersDone: {},        // {provider_id: true}
    result: null,
    bundlePath: "",
  };

  function render() {
    switch (state.phase) {
      case "loading": renderLoading(); break;
      case "providers": renderProviders(); break;
      case "shared": renderForm(state.sharedQuestions, t("ui.shared.title"), t("ui.shared.description"), null, submitShared); break;
      case "provider-form": renderProviderForm(); break;
      case "review": renderReview(); break;
      case "executing": renderExecuting(); break;
      case "result": renderResult(); break;
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

    // Load locales first, then providers
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
          state.providerForms[pf.provider_id] = pf;
        });
        state.sharedQuestions = data.shared_questions || [];

        // Initialize answer maps
        state.providers.forEach(function (p) {
          if (!state.answers[p.provider_id]) state.answers[p.provider_id] = {};
        });

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

        state.phase = "providers";
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

  // ── Provider list ──

  function renderProviders() {
    var allDone = state.providers.every(function (p) { return state.providersDone[p.provider_id]; });

    var html =
      '<div class="fade-in">' +
        '<div class="brand">' +
          '<div class="brand-icon">' +
            '<svg width="32" height="32" viewBox="0 0 32 32" fill="none"><rect width="32" height="32" rx="8" fill="#25c39e"/><path d="M10 16.5L14 20.5L22 12.5" stroke="white" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/></svg>' +
          '</div>' +
          '<h1 class="brand-title">' + esc(t("ui.title")) + '</h1>' +
          '<p class="brand-desc">' + esc(t("ui.description", [String(state.providers.length), state.bundlePath])) + '</p>' +
          renderLocalePicker() +
        '</div>' +
        '<div class="provider-list">';

    state.providers.forEach(function (p, idx) {
      var done = state.providersDone[p.provider_id];
      var form = state.providerForms[p.provider_id];
      var qCount = form ? form.questions.length : 0;
      var displayName = formatProviderName(p);
      html +=
        '<div class="provider-card">' +
          '<div class="prov-icon">' + esc(displayName.charAt(0)) + '</div>' +
          '<div>' +
            '<div class="prov-name">' + esc(displayName) + '</div>' +
            '<div class="prov-domain">' + esc(p.domain) + ' &middot; ' + qCount + ' ' + esc(t("ui.questions")) + '</div>' +
          '</div>' +
          '<span class="prov-badge ' + (done ? 'done' : 'pending') + '">' + (done ? esc(t("ui.done")) : esc(t("ui.pending"))) + '</span>' +
        '</div>';
    });

    html += '</div>';

    if (state.sharedQuestions.length > 0 && !state.sharedAnswersDone) {
      html += '<button class="btn btn-primary btn-lg" id="btn-start" style="width:100%">' + esc(t("ui.start_config")) + '</button>';
    } else if (!allDone) {
      var nextIdx = state.providers.findIndex(function (p) { return !state.providersDone[p.provider_id]; });
      html += '<button class="btn btn-primary btn-lg" id="btn-next-prov" data-idx="' + nextIdx + '" style="width:100%">' + esc(t("ui.configure", [formatProviderName(state.providers[nextIdx])])) + '</button>';
    } else {
      html += '<button class="btn btn-primary btn-lg" id="btn-review" style="width:100%">' + esc(t("ui.review_execute")) + '</button>';
    }

    html += '<div style="text-align:center;margin-top:.75rem"><button class="btn btn-ghost" id="btn-close-providers">' + esc(t("ui.close")) + '</button></div></div>';

    app.innerHTML = html;
    setupLocalePicker();

    var startBtn = document.getElementById("btn-start");
    if (startBtn) startBtn.addEventListener("click", function () {
      state.phase = "shared";
      render();
    });

    var nextBtn = document.getElementById("btn-next-prov");
    if (nextBtn) nextBtn.addEventListener("click", function () {
      state.currentProvider = parseInt(nextBtn.getAttribute("data-idx"), 10);
      state.phase = "provider-form";
      render();
    });

    var reviewBtn = document.getElementById("btn-review");
    if (reviewBtn) reviewBtn.addEventListener("click", function () {
      state.phase = "review";
      render();
    });

    document.getElementById("btn-close-providers").addEventListener("click", shutdown);
  }

  // ── Form rendering (reusable for shared + per-provider) ──

  function renderForm(questions, title, desc, backPhase, onSubmit) {
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
          '<div class="card-content">' +
            '<div id="form-area" class="form-fields">';

    var currentGroup = null;
    questions.forEach(function (q) {
      if (q.group && q.group !== currentGroup) {
        if (currentGroup !== null) html += '</div>'; // close previous group
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
            '</div>' +
          '</div>' +
          '<div class="card-footer">' +
            '<button class="btn btn-primary" id="btn-submit">' + esc(t("ui.continue")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;
    restoreFormValues(questions);
    setupVisibility(questions);

    document.getElementById("btn-back").addEventListener("click", function () {
      state.phase = backPhase || "providers";
      render();
    });

    document.getElementById("btn-submit").addEventListener("click", function () {
      if (!validateForm(questions)) return;
      collectFormValues(questions);
      onSubmit();
    });
  }

  function renderQuestion(q) {
    var visAttr = "";
    if (q.visible_if) {
      visAttr = ' data-vis-field="' + esc(q.visible_if.field) + '" data-vis-eq="' + esc(q.visible_if.eq || "") + '"';
    }

    var html = '<div class="field" id="field-' + esc(q.id) + '"' + visAttr + '>';

    if (q.kind === "Boolean") {
      html +=
        '<div class="field-row">' +
          '<label class="field-label" for="f-' + esc(q.id) + '">' + esc(q.title) + '</label>' +
          '<label class="switch">' +
            '<input type="checkbox" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '" />' +
            '<span class="switch-slider"></span>' +
          '</label>' +
        '</div>';
    } else {
      html += '<label class="field-label" for="f-' + esc(q.id) + '">' + esc(q.title);
      if (q.required) html += '<span class="required">*</span>';
      html += '</label>';

      var ph = q.placeholder || q.default_value || "";
      if (q.choices && q.choices.length > 0) {
        html += '<select id="f-' + esc(q.id) + '" name="' + esc(q.id) + '">';
        q.choices.forEach(function (c) {
          html += '<option value="' + esc(c) + '">' + esc(c) + '</option>';
        });
        html += '</select>';
      } else if (q.secret) {
        html += '<input type="password" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '"';
        if (ph) html += ' placeholder="' + esc(ph) + '"';
        html += ' />';
      } else {
        html += '<input type="text" id="f-' + esc(q.id) + '" name="' + esc(q.id) + '"';
        if (ph) html += ' placeholder="' + esc(ph) + '"';
        html += ' />';
      }

      if (q.help || q.docs_url) {
        html += '<div class="field-meta">';
        if (q.help) html += '<p class="field-help">' + esc(q.help) + '</p>';
        if (q.docs_url) html += '<a class="field-docs" href="' + esc(q.docs_url) + '" target="_blank" rel="noopener">Setup Guide ↗</a>';
        html += '</div>';
      }
    }

    html += '</div>';
    return html;
  }

  function restoreFormValues(questions) {
    // Determine which answer store to use
    var store = state.phase === "shared" ? state.sharedAnswers :
      (state.answers[state.providers[state.currentProvider].provider_id] || {});

    questions.forEach(function (q) {
      var el = document.getElementById("f-" + q.id);
      if (!el) return;
      var val = store[q.id];
      if (val !== undefined) {
        if (q.kind === "Boolean") {
          el.checked = val === true || val === "true";
        } else {
          el.value = val;
        }
      } else if (q.default_value && q.kind !== "Boolean") {
        el.value = q.default_value;
      } else if (q.kind === "Boolean" && q.default_value) {
        el.checked = q.default_value === "true" || q.default_value === true;
      }
    });
  }

  function collectFormValues(questions) {
    var store = state.phase === "shared" ? state.sharedAnswers :
      (state.answers[state.providers[state.currentProvider].provider_id] || {});

    questions.forEach(function (q) {
      var el = document.getElementById("f-" + q.id);
      if (!el) return;

      var fieldDiv = document.getElementById("field-" + q.id);
      var isHidden = fieldDiv && fieldDiv.style.display === "none";

      if (q.kind === "Boolean") {
        // Always collect booleans (even hidden ones keep their toggled state)
        store[q.id] = el.checked ? "true" : "false";
      } else if (!isHidden) {
        var val = el.value.trim();
        if (val) store[q.id] = val;
      }
    });

    if (state.phase === "shared") {
      state.sharedAnswers = store;
    } else {
      state.answers[state.providers[state.currentProvider].provider_id] = store;
    }
  }

  function setupVisibility(questions) {
    // Add change listeners on fields that other fields depend on
    var depFields = {};
    questions.forEach(function (q) {
      if (q.visible_if) depFields[q.visible_if.field] = true;
    });

    Object.keys(depFields).forEach(function (fid) {
      var el = document.getElementById("f-" + fid);
      if (el) {
        var handler = function () { evaluateVisibility(); };
        el.addEventListener("change", handler);
        el.addEventListener("input", handler);
      }
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
      var val = el.value.trim();
      if (q.required && !val && !q.default_value) {
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

  // ── Shared questions submit ──

  function submitShared() {
    state.sharedAnswersDone = true;
    // Apply shared answers to all providers
    state.providers.forEach(function (p) {
      var store = state.answers[p.provider_id] || {};
      Object.keys(state.sharedAnswers).forEach(function (k) {
        if (!store[k]) store[k] = state.sharedAnswers[k];
      });
      state.answers[p.provider_id] = store;
    });
    state.currentProvider = 0;
    state.phase = "provider-form";
    render();
  }

  // ── Per-provider form ──

  function renderProviderForm() {
    var p = state.providers[state.currentProvider];
    var form = state.providerForms[p.provider_id];
    if (!form || form.questions.length === 0) {
      state.providersDone[p.provider_id] = true;
      advanceProvider();
      return;
    }
    renderForm(
      form.questions,
      form.title || formatProviderName(p),
      t("ui.provider.configure", [formatProviderName(p)]),
      "providers",
      function () {
        state.providersDone[p.provider_id] = true;
        advanceProvider();
      }
    );
  }

  function advanceProvider() {
    var next = state.providers.findIndex(function (p) { return !state.providersDone[p.provider_id]; });
    if (next >= 0) {
      state.currentProvider = next;
      state.phase = "provider-form";
    } else {
      state.phase = "review";
    }
    render();
  }

  // ── Review ──

  function renderReview() {
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

    state.providers.forEach(function (p) {
      var answers = state.answers[p.provider_id] || {};
      var form = state.providerForms[p.provider_id];
      var keys = Object.keys(answers);
      if (keys.length === 0) return;

      html += '<div class="review-group"><h4 class="review-group-title">' + esc(formatProviderName(p)) + '</h4>';
      keys.forEach(function (k) {
        var val = answers[k];
        var isSecret = form && form.questions.some(function (q) { return q.id === k && q.secret; });
        var display;
        if (typeof val === "boolean") {
          display = val ? t("ui.review.yes") : t("ui.review.no");
        } else if (isSecret && val) {
          display = t("ui.review.secret_mask");
        } else {
          display = String(val || "");
        }
        if (!display) return;
        html +=
          '<div class="review-item">' +
            '<span class="review-key">' + esc(k) + '</span>' +
            '<span class="review-val' + (isSecret ? ' secret' : '') + '">' + esc(display) + '</span>' +
          '</div>';
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
    document.getElementById("btn-back-review").addEventListener("click", function () {
      state.phase = "providers";
      render();
    });
    document.getElementById("btn-execute").addEventListener("click", executeSetup);
  }

  // ── Execute ──

  function executeSetup() {
    state.phase = "executing";
    render();
    fetch("/api/execute", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ answers: state.answers }),
    })
    .then(function (r) { return r.json(); })
    .then(function (result) {
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
      '<div class="fade-in">' +
        '<div class="card">' +
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
        instr.steps.forEach(function (step) {
          var text = step.replace(/^\d+\.\s*/, '');
          html += '<li>' + esc(text) + '</li>';
        });
        html += '</ol></div>';
      });
      html += '</div>';
    }

    if (r && r.stdout) {
      html += '<div class="output-section"><h4 class="output-title">' + esc(t("ui.result.output")) + '</h4><pre class="output-pre">' + esc(r.stdout) + '</pre></div>';
    }
    if (r && r.stderr) {
      html += '<div class="output-section"><h4 class="output-title">' + esc(t("ui.result.log")) + '</h4><pre class="output-pre stderr">' + esc(r.stderr) + '</pre></div>';
    }

    html +=
          '</div>' +
          '<div class="card-footer card-footer-split">' +
            '<button class="btn btn-secondary" id="btn-restart">' + esc(t("ui.new_setup")) + '</button>' +
            '<button class="btn btn-ghost" id="btn-close">' + esc(t("ui.close")) + '</button>' +
          '</div>' +
        '</div>' +
      '</div>';

    app.innerHTML = html;
    document.getElementById("btn-restart").addEventListener("click", function () {
      state.phase = "loading";
      state.providersDone = {};
      state.sharedAnswersDone = false;
      render();
    });
    document.getElementById("btn-close").addEventListener("click", shutdown);
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

  function formatProviderName(provider) {
    if (typeof provider === "object" && provider.display_name) return provider.display_name;
    var id = typeof provider === "object" ? provider.provider_id : provider;
    var name = id.replace(/^messaging-/, "").replace(/^events-/, "").replace(/^state-/, "");
    return name.split("-").map(function (w) {
      if (w === "gui") return "GUI";
      if (w === "api") return "API";
      return w.charAt(0).toUpperCase() + w.slice(1);
    }).join(" ");
  }

  render();
})();
