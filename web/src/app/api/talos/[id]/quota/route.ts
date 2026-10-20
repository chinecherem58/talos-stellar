import { db } from "@/db";
import { tlsTalos, tlsApprovals, tlsRevenues } from "@/db/schema";
import { and, eq, gte, sql } from "drizzle-orm";

// GET /api/talos/:id/quota — Spending quota and GTM budget utilisation
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
        approvalThreshold: tlsTalos.approvalThreshold,
        gtmBudget: tlsTalos.gtmBudget,
      })
      .from(tlsTalos)
      .where(eq(tlsTalos.id, id))
      .limit(1)
      .then((r) => r[0] ?? null);

    if (!talos) {
      return Response.json({ error: "TALOS not found" }, { status: 404 });
    }

    const approvalThreshold = Number(talos.approvalThreshold);
    const gtmBudget = Number(talos.gtmBudget);

    const now = new Date();
    const thirtyDaysAgo = new Date(now.getTime() - 30 * 24 * 60 * 60 * 1000);

    // Approved spending in the last 30 days (realised spend)
    const [approvedSpend] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
        count: sql<number>`count(*)::int`,
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

    // Pending approvals (committed but not yet approved/rejected)
    const [pendingSpend] = await db
      .select({
        total: sql<string>`coalesce(sum(${tlsApprovals.amount}), '0')`,
        count: sql<number>`count(*)::int`,
      })
      .from(tlsApprovals)
      .where(
        and(
          eq(tlsApprovals.talosId, id),
          eq(tlsApprovals.status, "pending"),
          sql`${tlsApprovals.amount} is not null`
        )
      );

    // Revenue over the same 30-day window (for coverage ratio)
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

    const approvedSpendNum = Number(approvedSpend.total);
    const pendingSpendNum = Number(pendingSpend.total);
    const revenue30dNum = Number(revenue30d.total);

    // GTM budget utilisation: percentage of 30-day budget consumed
    const gtmUtilisation =
      gtmBudget > 0
        ? Math.min((approvedSpendNum / gtmBudget) * 100, 100)
        : 0;

    // Remaining budget headroom
    const remainingBudget = Math.max(0, gtmBudget - approvedSpendNum);

    // Coverage ratio: approved spending vs revenue (health signal)
    const spendCoverageRatio =
      revenue30dNum > 0
        ? Math.round((approvedSpendNum / revenue30dNum) * 10000) / 100
        : null;

    // Whether the next approval request would exceed the per-transaction threshold
    const singleTransactionLimitRemaining = approvalThreshold;

    const status =
      gtmUtilisation >= 100
        ? "over_budget"
        : gtmUtilisation >= 80
        ? "near_limit"
        : "healthy";

    return Response.json({
      talosId: talos.id,
      talosName: talos.name,
      generatedAt: now.toISOString(),

      quota: {
        approvalThreshold,
        gtmBudget,
        singleTransactionLimitRemaining,
      },

      utilisation: {
        approvedSpend30d: Math.round(approvedSpendNum * 100) / 100,
        pendingSpend: Math.round(pendingSpendNum * 100) / 100,
        remainingBudget: Math.round(remainingBudget * 100) / 100,
        gtmUtilisationPct: Math.round(gtmUtilisation * 100) / 100,
        approvedApprovals30d: approvedSpend.count,
        pendingApprovals: pendingSpend.count,
      },

      revenue: {
        total30d: Math.round(revenue30dNum * 100) / 100,
        spendCoverageRatioPct: spendCoverageRatio,
      },

      status,
      statusMessage:
        status === "over_budget"
          ? "⚠ GTM budget exhausted — new spending requires patron approval"
          : status === "near_limit"
          ? "⚠ Approaching budget limit (≥80 % consumed)"
          : "✓ Spending within quota",
    });
  } catch (err) {
    console.error("[quota GET]", err);
    return Response.json({ error: "Internal server error" }, { status: 500 });
  }
}
