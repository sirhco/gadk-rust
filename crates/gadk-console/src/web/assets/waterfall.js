/* gadk console: waterfall bars, subtree collapse, the span detail pane, payload expand,
   and confirm() before submitting forms marked data-confirm.
   A plain script loaded with defer. Every feature first looks for its elements, so the file
   is harmless on pages without them. Text reaches the page only through textContent. */
(function () {
  "use strict";

  function depthOf(row) {
    return Number(row.getAttribute("data-depth")) || 0;
  }

  function make(tag, className, text) {
    var el = document.createElement(tag);
    if (className) {
      el.className = className;
    }
    if (text !== undefined && text !== null) {
      el.textContent = String(text);
    }
    return el;
  }

  /* Bars: left and width from data-offset / data-width (percent), via CSSOM. */
  function positionBars(rows) {
    rows.forEach(function (row) {
      var bar = row.querySelector(".bar");
      if (!bar) {
        return;
      }
      var offset = parseFloat(row.getAttribute("data-offset"));
      var width = parseFloat(row.getAttribute("data-width"));
      bar.style.left = (isFinite(offset) ? offset : 0) + "%";
      bar.style.width = (isFinite(width) ? width : 0) + "%";
    });
  }

  /* The rows after rows[i] that are deeper than it: its subtree, in tree order. */
  function subtree(rows, i) {
    var depth = depthOf(rows[i]);
    var out = [];
    for (var j = i + 1; j < rows.length && depthOf(rows[j]) > depth; j++) {
      out.push(rows[j]);
    }
    return out;
  }

  function toggle(rows, i) {
    var row = rows[i];
    var collapsed = row.classList.toggle("collapsed");
    var mark = row.querySelector(".toggle");
    if (mark) {
      mark.textContent = collapsed ? "▸" : "▾";
    }
    var below = subtree(rows, i);
    if (collapsed) {
      below.forEach(function (r) {
        r.classList.add("hidden");
      });
      return;
    }
    /* Expanding: show the subtree again, except under descendants still collapsed. */
    var hideDeeperThan = Infinity;
    below.forEach(function (r) {
      var d = depthOf(r);
      if (d > hideDeeperThan) {
        r.classList.add("hidden");
        return;
      }
      hideDeeperThan = r.classList.contains("collapsed") ? d : Infinity;
      r.classList.remove("hidden");
    });
  }

  function readData() {
    var block = document.getElementById("trace-data");
    if (!block) {
      return null;
    }
    try {
      return JSON.parse(block.textContent);
    } catch (e) {
      return null;
    }
  }

  function showSpan(data, id) {
    var pane = document.getElementById("span-detail");
    if (!pane || !data || !Array.isArray(data.spans)) {
      return;
    }
    var span = null;
    data.spans.forEach(function (s) {
      if (s.id === id) {
        span = s;
      }
    });
    while (pane.firstChild) {
      pane.removeChild(pane.firstChild);
    }
    if (!span) {
      return;
    }
    pane.appendChild(make("h2", null, span.name));
    var dl = make("dl");
    function item(key, value) {
      dl.appendChild(make("dt", null, key));
      dl.appendChild(make("dd", null, value));
    }
    item("span id", span.id);
    item("parent", span.parent || "(none)");
    item("start", span.start);
    item("duration", Number(span.duration_ms).toFixed(1) + " ms");
    var labels = span.labels || {};
    Object.keys(labels).sort().forEach(function (key) {
      item(key, labels[key]);
    });
    pane.appendChild(dl);

    /* The span's log entries: copies of the server-rendered ones under #logs-<id>, so the
       page carries each payload once. */
    var section = document.getElementById("logs-" + id);
    var entries = section ? Array.prototype.slice.call(section.querySelectorAll(".log-entry")) : [];
    pane.appendChild(make("h3", null, entries.length === 1 ? "1 log entry" : entries.length + " log entries"));
    entries.forEach(function (entry) {
      pane.appendChild(entry.cloneNode(true));
    });
  }

  function initWaterfall() {
    var rows = Array.prototype.slice.call(document.querySelectorAll(".span-row"));
    if (rows.length === 0) {
      return;
    }
    positionBars(rows);
    var data = readData();
    rows.forEach(function (row, i) {
      row.addEventListener("click", function (event) {
        var target = event.target;
        if (target.closest("a")) {
          return; /* the prompt link navigates */
        }
        if (target.closest(".toggle") && subtree(rows, i).length > 0) {
          toggle(rows, i);
          return;
        }
        rows.forEach(function (r) {
          r.classList.remove("selected");
        });
        row.classList.add("selected");
        showSpan(data, row.getAttribute("data-span"));
      });
    });
  }

  /* Payload expand, for server-rendered and pane-built payloads alike. */
  document.addEventListener("click", function (event) {
    var target = event.target;
    var button = target && target.closest ? target.closest("button.expand") : null;
    var box = button ? button.closest(".payload") : null;
    if (!box) {
      return;
    }
    box.classList.remove("collapsed");
    box.classList.add("expanded");
  });

  /* Label forms ask first (a convenience, not a security control). */
  document.addEventListener("submit", function (event) {
    var form = event.target;
    if (!form || !form.hasAttribute || !form.hasAttribute("data-confirm")) {
      return;
    }
    if (!window.confirm(form.getAttribute("data-confirm"))) {
      event.preventDefault();
    }
  });

  initWaterfall();
})();
