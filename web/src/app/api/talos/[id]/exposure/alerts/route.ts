import { db } from "@/db";
import { tlsTalos, tlsCommerceJobs, tlsRevenues, tlsApprovals } from "@/db/schema";
import { and, eq, gte, sql } from "drizzle-orm";

export type AlertSeverity = "critical" | "warning" | "info";

export interface ExposureAlert {
  id: string;
  severity: AlertSeverity;
  code: string;
  title: string;
  message: string;
  value?: number;
  threshold?: number;
}

// GET /api/talos/:id/exposure/alerts — Active risk alerts for a Talos
export async function GET(
  _request: Request,
  { params }: { params: Promise<{ id: string }> }
) {
  const { id } = await params;

  try {
    const talos = await db
      .select({
        id: tlsTalos.id,
        name: tlsTalos.name,
        gtmBudget: tlsTalos.gtmBudget,
        approvalThreshold: tlsTalos.approvalThreshold,
      })
      .from(tlsTalos)
      .where(eq(tlsTalos.id, id))
      .limit(1)
      .then((r) => r[0] ?? null);

    if (!talos) {
      return Response.json({ error: "TALOS not found" }, { status: 404 });
    }

    const now = new Date();
    const thirtyDaysAgo = new Date(now.getTime() - 30 * 24 * 60 * 60 * 1000);
    const sevenDaysAgo = new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000);

    const gtmBudget = Number(talos.gtmBudget);

    // ── Budget utilisation ────────────────────────────────────────────
    const [approvedSpend30d] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
      })
      .from(tlsApprovals)
      .where(
        and(
          eq(tlsApprovals.talosId, id),
          eq(tlsApprovals.status, "approved"),
          sql`${tlsApprovals.amount} is not null`,
          gte(tlsApprovals.createdAt, thirtyDaysAgo)
        )
      );

    // ── Stale open jobs (pending > 7 days) ───────────────────────────
    const [staleJobs] = await db
      .select({
        count: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsCommerceJobs.amount}), '0')`,
      })
      .from(tlsCommerceJobs)
      .where(
        and(
          eq(tlsCommerceJobs.talosId, id),
          eq(tlsCommerceJobs.status, "pending"),
          sql`${tlsCommerceJobs.createdAt} < ${sevenDaysAgo}`
        )
      );

    // ── Failed jobs last 30 days ──────────────────────────────────────
    const [failedJobs] = await db
      .select({
        count: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsCommerceJobs.amount}), '0')`,
      })
      .from(tlsCommerceJobs)
      .where(
        and(
          eq(tlsCommerceJobs.talosId, id),
          eq(tlsCommerceJobs.status, "failed"),
          gte(tlsCommerceJobs.createdAt, thirtyDaysAgo)
        )
      );

    // ── Revenue trend (7-day vs 30-day) for revenue drop alert ───────
    const [revenue7d] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsRevenues.amount}), '0')`,
        count: sql<number>`count(*)::int`,
      })
      .from(tlsRevenues)
      .where(
        and(
          eq(tlsRevenues.talosId, id),
          gte(tlsRevenues.createdAt, sevenDaysAgo)
        )
      );

    const [revenue30d] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsRevenues.amount}), '0')`,
      })
      .from(tlsRevenues)
      .where(
        and(
          eq(tlsRevenues.talosId, id),
          gte(tlsRevenues.createdAt, thirtyDaysAgo)
        )
      );

    // ── Revenue concentration (largest single source / total) ─────────
    const revenueBySrc = await db
      .select({
        source: tlsRevenues.source,
        total: sql<string>`coalesce(sum(${tlsRevenues.amount}), '0')`,
      })
      .from(tlsRevenues)
      .where(
        and(
          eq(tlsRevenues.talosId, id),
          gte(tlsRevenues.createdAt, thirtyDaysAgo)
        )
      )
      .groupBy(tlsRevenues.source);

    const totalRev30d = revenueBySrc.reduce((s, r) => s + Number(r.total), 0);
    const maxSrcRev = revenueBySrc.reduce(
      (m, r) => Math.max(m, Number(r.total)),
      0
    );
    const concentrationPct =
      totalRev30d > 0 ? (maxSrcRev / totalRev30d) * 100 : 0;

    // ── Pending liabilities ───────────────────────────────────────────
    const [pendingLiabilities] = await db
      .select({
        count: sql<number>`count(*)::int`,
        totalAmount: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
      })
      .from(tlsApprovals)
      .where(
        and(
          eq(tlsApprovals.talosId, id),
          eq(tlsApprovals.status, "pending"),
          sql`${tlsApprovals.amount} is not null`
        )
      );

    // ── Build alerts ─────────────────────────────────────────────────
    const alerts: ExposureAlert[] = [];
    const approvedSpend = Number(approvedSpend30d.total);
    const gtmUtilPct = gtmBudget > 0 ? (approvedSpend / gtmBudget) * 100 : 0;

    if (gtmUtilPct >= 100) {
      alerts.push({
        id: "budget_exhausted",
        severity: "critical",
        code: "BUDGET_EXHAUSTED",
        title: "GTM budget exhausted",
        message: `30-day approved spending (${approvedSpend.toFixed(2)} USDC) has exceeded the GTM budget of ${gtmBudget} USDC.`,
        value: Math.round(approvedSpend * 100) / 100,
        threshold: gtmBudget,
      });
    } else if (gtmUtilPct >= 80) {
      alerts.push({
        id: "budget_near_limit",
        severity: "warning",
        code: "BUDGET_NEAR_LIMIT",
        title: "Approaching GTM budget limit",
        message: `${gtmUtilPct.toFixed(1)} % of the GTM budget consumed in the last 30 days.`,
        value: Math.round(gtmUtilPct * 100) / 100,
        threshold: 80,
      });
    }

    if (staleJobs.count > 0) {
      alerts.push({
        id: "stale_jobs",
        severity: "warning",
        code: "STALE_OPEN_JOBS",
        title: "Stale open commerce jobs",
        message: `${staleJobs.count} job(s) have been pending for more than 7 days, totalling ${Number(staleJobs.totalAmount).toFixed(2)} USDC.`,
        value: staleJobs.count,
      });
    }

    if (failedJobs.count > 5) {
      alerts.push({
        id: "high_failure_rate",
        severity: failedJobs.count > 20 ? "critical" : "warning",
        code: "HIGH_JOB_FAILURE_RATE",
        title: "Elevated job failure rate",
        message: `${failedJobs.count} job(s) failed in the last 30 days (${Number(failedJobs.totalAmount).toFixed(2)} USDC at risk).`,
        value: failedJobs.count,
        threshold: 5,
      });
    }

    // Revenue drop: 7-day weekly run-rate vs 30-day average week
    const weeklyAvg30d = Number(revenue30d.total) / 4;
    const rev7dNum = Number(revenue7d.total);
    if (weeklyAvg30d > 0 && rev7dNum < weeklyAvg30d * 0.5) {
      alerts.push({
        id: "revenue_drop",
        severity: "warning",
        code: "REVENUE_DROP",
        title: "Revenue drop detected",
        message: `7-day revenue (${rev7dNum.toFixed(2)} USDC) is less than half the 30-day weekly average (${weeklyAvg30d.toFixed(2)} USDC).`,
        value: Math.round(rev7dNum * 100) / 100,
        threshold: Math.round(weeklyAvg30d * 50) / 100,
      });
    }

    if (concentrationPct > 80 && totalRev30d > 0) {
      alerts.push({
        id: "revenue_concentration",
        severity: "info",
        code: "REVENUE_CONCENTRATION",
        title: "High revenue source concentration",
        message: `${concentrationPct.toFixed(1)} % of 30-day revenue comes from a single source.`,
        value: Math.round(concentrationPct * 100) / 100,
        threshold: 80,
      });
    }

    if (Number(pendingLiabilities.totalAmount) > gtmBudget * 0.5 && gtmBudget > 0) {
      alerts.push({
        id: "pending_liabilities",
        severity: "info",
        code: "HIGH_PENDING_LIABILITIES",
        title: "High pending liability exposure",
        message: `${pendingLiabilities.count} pending approval(s) totalling ${Number(pendingLiabilities.totalAmount).toFixed(2)} USDC represent > 50 % of the GTM budget.`,
        value: Math.round(Number(pendingLiabilities.totalAmount) * 100) / 100,
        threshold: Math.round(gtmBudget * 50) / 100,
      });
    }

    const criticalCount = alerts.filter((a) => a.severity === "critical").length;
    const warningCount = alerts.filter((a) => a.severity === "warning").length;

    return Response.json({
      talosId: talos.id,
      talosName: talos.name,
      generatedAt: now.toISOString(),
      alertCount: alerts.length,
      criticalCount,
      warningCount,
      infoCount: alerts.length - criticalCount - warningCount,
      alerts,
    });
  } catch (err) {
    console.error("[exposure/alerts GET]", err);
    return Response.json({ error: "Internal server error" }, { status: 500 });
  }
}
