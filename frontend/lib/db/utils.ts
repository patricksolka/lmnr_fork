import { db } from "./drizzle";
import { eq, and, gt, sql, lt, SQL, lte, ne, gte, BinaryOperator, getTableColumns } from "drizzle-orm";
import { membersOfWorkspaces, projects, users } from "./schema";
import { getServerSession } from 'next-auth';
import { authOptions } from "../auth";
import { PgTableWithColumns, TableConfig } from "drizzle-orm/pg-core";
import { PaginatedResponse } from "../types";

export const isCurrentUserMemberOfProject = async (projectId: string) => {
  const session = await getServerSession(authOptions);
  const user = session?.user;

  if (!user) {
    return false;
  }

  const result = await db
    .select({ userId: users.id })
    .from(users)
    .innerJoin(membersOfWorkspaces, eq(users.id, membersOfWorkspaces.userId))
    .innerJoin(projects, eq(membersOfWorkspaces.workspaceId, projects.workspaceId))
    .where(and(
      eq(users.email, user.email!),
      eq(projects.id, projectId)
    ))
    .limit(1);

  return result.length > 0;
};

export const isCurrentUserMemberOfWorkspace = async (workspaceId: string) => {
  const session = await getServerSession(authOptions);
  const user = session?.user;

  if (!user) {
    return false;
  }

  const result = await db
    .select({ userId: users.id })
    .from(users)
    .innerJoin(membersOfWorkspaces, eq(users.id, membersOfWorkspaces.userId))
    .where(and(
      eq(users.email, user.email!),
      eq(membersOfWorkspaces.workspaceId, workspaceId)
    ))
    .limit(1);

  return result.length > 0;
};

export const getDateRangeFilters = (
  startTime: string | null,
  endTime: string | null,
  pastHours: string | null
): SQL[] => {
  if (pastHours && !isNaN(parseFloat(pastHours))) {
    // sql.raw is a concious choice, because `sql` operator will bind the value as a query
    // parameter, which postgres driver will reject as it cannot infer the data type.
    return [gt(sql`start_time`, sql.raw(`NOW() - INTERVAL '${parseFloat(pastHours)} HOUR'`))];
  }
  if (startTime) {
    return [gt(sql`end_time`, startTime), lt(sql`end_time`, endTime ?? sql`NOW()`)];
  }
  return [];
};

interface PaginatedGetParams<T extends TableConfig, BT extends TableConfig, R> {
  table: PgTableWithColumns<T>;
  pageNumber: number;
  pageSize: number;
  baseFilters: SQL[];
  filters: SQL[];
  orderBy: SQL;
  additionalColumns?: Record<string, SQL.Aliased<any>>;
  baseTable?: PgTableWithColumns<BT>;
}

export const paginatedGet = async<T extends TableConfig, BT extends TableConfig, R> (
  {
    table,
    pageNumber,
    pageSize,
    baseFilters,
    filters,
    orderBy,
    additionalColumns,
    baseTable,
  }: PaginatedGetParams<T, BT, R>
): Promise<PaginatedResponse<R>> => {
  const allFilters = baseFilters.concat(filters);

  const baseFiltered = db.$with(
    "base",
  ).as(
    db
      .select({
        ...getTableColumns(table),
        ...additionalColumns,
      })
      .from(table)
      .where(and(...baseFilters))
  );

  const itemsQuery = db
    .with(baseFiltered)
    .select()
    .from(baseFiltered)
    .where(and(...allFilters))
    .orderBy(orderBy)
    .limit(pageSize)
    .offset(pageNumber * pageSize);

  const countQueries = async () => {
    const totalCount = await db
      .with(baseFiltered)
      .select({ count: sql<number>`COUNT(*)` })
      .from(baseFiltered)
      .where(and(...allFilters))
      .then(([{ count }]) => count);
    const anyInProject = totalCount > 0
      ? true
      : await db.$count(baseTable ?? table, and(...baseFilters)) > 0;
    return { totalCount, anyInProject };
  };

  const [items, { totalCount, anyInProject }] = await Promise.all([
    itemsQuery,
    countQueries(),
  ]);

  return { items: items as R[], totalCount, anyInProject };
};
